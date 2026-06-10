/// CLI tests (Phase 10).
///
/// Verifies that the CLI struct parses correctly and that the `version_string`
/// helper produces the expected output.
use claude_print::cli::{version_string, Cli, OutputFormat};
use clap::Parser;

// ── OutputFormat Display ──────────────────────────────────────────────────────

#[test]
fn output_format_display_text() {
    assert_eq!(format!("{}", OutputFormat::Text), "text");
}

#[test]
fn output_format_display_json() {
    assert_eq!(format!("{}", OutputFormat::Json), "json");
}

#[test]
fn output_format_display_stream_json() {
    assert_eq!(format!("{}", OutputFormat::StreamJson), "stream-json");
}

// ── version_string format ─────────────────────────────────────────────────────

#[test]
fn version_string_contains_claude_print_prefix() {
    let s = version_string(Some("2.1.168"));
    assert!(
        s.starts_with("claude-print "),
        "version string must start with 'claude-print '; got: {s:?}"
    );
}

#[test]
fn version_string_contains_wrapping_claude() {
    let s = version_string(Some("2.1.168"));
    assert!(
        s.contains("wrapping claude 2.1.168"),
        "version string must contain 'wrapping claude <version>'; got: {s:?}"
    );
}

#[test]
fn version_string_not_found() {
    let s = version_string(None);
    assert!(
        s.contains("not found"),
        "version string with None must say 'not found'; got: {s:?}"
    );
}

#[test]
fn version_string_format_is_parseable() {
    let s = version_string(Some("2.0.0"));
    // Must match: "claude-print X.Y.Z (wrapping claude A.B.C)"
    assert!(s.contains('('), "must have opening paren");
    assert!(s.contains(')'), "must have closing paren");
    let inside: &str = s.split('(').nth(1).and_then(|s| s.split(')').next()).unwrap_or("");
    assert!(
        inside.starts_with("wrapping claude"),
        "content in parens must start with 'wrapping claude'; got: {inside:?}"
    );
}

// ── CLI argument parsing ──────────────────────────────────────────────────────

#[test]
fn cli_positional_prompt_parsed() {
    let cli = Cli::try_parse_from(["claude-print", "hello world"]).unwrap();
    assert_eq!(cli.prompt.as_deref(), Some("hello world"));
}

#[test]
fn cli_no_prompt_gives_none() {
    let cli = Cli::try_parse_from(["claude-print"]).unwrap();
    assert!(cli.prompt.is_none(), "no positional arg should give None prompt");
}

#[test]
fn cli_output_format_default_is_text() {
    let cli = Cli::try_parse_from(["claude-print"]).unwrap();
    assert!(matches!(cli.output_format, OutputFormat::Text));
}

#[test]
fn cli_output_format_json() {
    let cli =
        Cli::try_parse_from(["claude-print", "--output-format", "json"]).unwrap();
    assert!(matches!(cli.output_format, OutputFormat::Json));
}

#[test]
fn cli_output_format_stream_json() {
    let cli =
        Cli::try_parse_from(["claude-print", "--output-format", "stream-json"]).unwrap();
    assert!(matches!(cli.output_format, OutputFormat::StreamJson));
}

#[test]
fn cli_output_format_invalid_returns_error() {
    let result = Cli::try_parse_from(["claude-print", "--output-format", "xml"]);
    assert!(result.is_err(), "invalid output format must produce a parse error");
}

#[test]
fn cli_no_inherit_hooks_flag() {
    let cli = Cli::try_parse_from(["claude-print", "--no-inherit-hooks"]).unwrap();
    assert!(cli.no_inherit_hooks, "--no-inherit-hooks must set no_inherit_hooks=true");
}

#[test]
fn cli_no_inherit_hooks_defaults_false() {
    let cli = Cli::try_parse_from(["claude-print"]).unwrap();
    assert!(!cli.no_inherit_hooks, "no_inherit_hooks must default to false");
}

#[test]
fn cli_verbose_flag() {
    let cli = Cli::try_parse_from(["claude-print", "--verbose"]).unwrap();
    assert!(cli.verbose);
}

#[test]
fn cli_dangerously_skip_permissions_flag() {
    let cli =
        Cli::try_parse_from(["claude-print", "--dangerously-skip-permissions"]).unwrap();
    assert!(cli.dangerously_skip_permissions);
}

#[test]
fn cli_max_turns_default_is_30() {
    let cli = Cli::try_parse_from(["claude-print"]).unwrap();
    assert_eq!(cli.max_turns, 30, "max_turns must default to 30");
}

#[test]
fn cli_max_turns_custom() {
    let cli = Cli::try_parse_from(["claude-print", "--max-turns", "5"]).unwrap();
    assert_eq!(cli.max_turns, 5);
}

#[test]
fn cli_timeout_default_is_3600() {
    let cli = Cli::try_parse_from(["claude-print"]).unwrap();
    assert_eq!(cli.timeout, 3600, "timeout must default to 3600");
}

#[test]
fn cli_input_file_flag() {
    let cli =
        Cli::try_parse_from(["claude-print", "--input-file", "/tmp/prompt.txt"]).unwrap();
    assert_eq!(
        cli.input_file.as_deref(),
        Some(std::path::Path::new("/tmp/prompt.txt"))
    );
}

#[test]
fn cli_claude_binary_flag() {
    let cli =
        Cli::try_parse_from(["claude-print", "--claude-binary", "/usr/bin/claude"]).unwrap();
    assert_eq!(
        cli.claude_binary.as_deref(),
        Some(std::path::Path::new("/usr/bin/claude"))
    );
}

#[test]
fn cli_model_flag() {
    let cli = Cli::try_parse_from(["claude-print", "--model", "claude-opus-4-8"]).unwrap();
    assert_eq!(cli.model.as_deref(), Some("claude-opus-4-8"));
}
