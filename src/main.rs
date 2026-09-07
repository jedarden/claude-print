use clap::Parser;
use claude_print::cli::{version_string, Cli};
use claude_print::config::Config;
use claude_print::emitter;
use claude_print::error::{ClaudePrintError, Error};
use claude_print::hook;
use claude_print::prompt;
use claude_print::session;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process;
use std::time::Instant;

fn resolve_claude_version(binary: Option<&std::path::Path>) -> Option<String> {
    let binary = binary
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("claude"));

    let output = std::process::Command::new(&binary)
        .arg("--version")
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr);
    let first_line = combined.lines().next()?;
    Some(first_line.trim().to_string())
}

/// Exit with cleanup, ensuring temp dir is removed before process::exit().
fn exit_with_cleanup(code: i32) -> ! {
    session::cleanup_temp_dir();
    process::exit(code);
}

fn main() {
    // Register the cleanup handler early to ensure it runs on all exit paths,
    // including external signals that trigger Rust's default handler.
    session::register_cleanup_handler();

    let cli = Cli::parse();

    // Ordinary invocations retain the automatic best-effort orphan sweep.
    // Check mode owns its scan so plain --check stays warn-only and only an
    // explicit --check --clean removes the directories it reports.
    if !cli.check {
        hook::cleanup_orphans();
    }

    // HOME is a process-wide prerequisite, including for early-exit entry
    // points such as --version. Validate it before dispatch so the CLI,
    // config, poller, and direct Session callers share one strict contract.
    if let Err(error) = claude_print::util::get_home() {
        let mut stdout = io::stdout().lock();
        let mut stderr = io::stderr().lock();
        let _ = emit_error(
            &mut stdout,
            &mut stderr,
            &ClaudePrintError::Setup(error.to_string()),
            &cli.output_format,
            "unknown",
            true,
        );
        exit_with_cleanup(2);
    }

    if cli.version {
        let claude_version = resolve_claude_version(cli.claude_binary.as_deref());
        println!("{}", version_string(claude_version.as_deref()));
        exit_with_cleanup(0);
    }

    if cli.check {
        let code = claude_print::check::run_with_clean(cli.claude_binary.as_deref(), cli.clean);
        exit_with_cleanup(code);
    }

    // Resolve the claude binary path
    let claude_bin = cli
        .claude_binary
        .clone()
        .unwrap_or_else(|| PathBuf::from("claude"));

    // AS-5: Check if claude binary exists before calling session::run(). In text
    // mode this is a human-readable stderr message (unchanged). In JSON /
    // stream-json modes it must surface as a structured `result` object with
    // is_error:true on stdout — the same shape every other error arm emits via
    // emit_error — so a missing binary does not leave JSON callers with empty
    // stdout (the binary_e2e AS-5 json regression guard asserts this).
    if which::which(&claude_bin).is_err() {
        let not_found_msg = format!("'{}' not found in PATH", claude_bin.to_string_lossy());
        if matches!(cli.output_format, claude_print::cli::OutputFormat::Text) {
            eprintln!("claude-print: {}", not_found_msg);
        } else {
            let mut stdout = io::stdout().lock();
            let mut stderr = io::stderr().lock();
            let _ = emit_error(
                &mut stdout,
                &mut stderr,
                &ClaudePrintError::Setup(not_found_msg),
                &cli.output_format,
                &resolve_claude_version(cli.claude_binary.as_deref())
                    .unwrap_or_else(|| "unknown".to_string()),
                true,
            );
        }
        exit_with_cleanup(2);
    }

    // Prompt resolution (in order of precedence)
    let prompt_bytes = if let Some(ref input_file) = cli.input_file {
        // --input-file <path>: resolve to an absolute path and size/type-check
        // it BEFORE slurping the contents (plan Security > T-2), then read.
        let resolved = match prompt::resolve_input_file(input_file) {
            Ok(p) => p,
            Err(prompt::InputFileError::TooLarge {
                resolved,
                size,
                limit,
            }) => {
                eprintln!(
                    "claude-print: input file '{}' is {} bytes, which exceeds the {}-byte limit",
                    resolved.display(),
                    size,
                    limit
                );
                exit_with_cleanup(2);
            }
            Err(prompt::InputFileError::NotRegularFile { resolved }) => {
                eprintln!(
                    "claude-print: input file '{}' is not a regular file",
                    resolved.display()
                );
                exit_with_cleanup(4);
            }
            Err(prompt::InputFileError::ResolveFailed { path, source }) => {
                eprintln!(
                    "claude-print: failed to resolve input file '{}': {}",
                    path.display(),
                    source
                );
                exit_with_cleanup(4);
            }
        };
        match std::fs::read(&resolved) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!(
                    "claude-print: failed to read input file '{}': {}",
                    resolved.display(),
                    e
                );
                exit_with_cleanup(4);
            }
        }
    } else if let Some(ref prompt_str) = cli.prompt {
        // positional <prompt>: encode as UTF-8 bytes
        prompt_str.as_bytes().to_vec()
    } else {
        // stdin (when !stdin.is_terminal())
        if !atty::is(atty::Stream::Stdin) {
            match prompt::read_stdin_with_limit() {
                Ok(buffer) => {
                    if buffer.is_empty() {
                        eprintln!(
                            "claude-print: no prompt provided (pass as argument, --input-file, or stdin)"
                        );
                        exit_with_cleanup(4);
                    }
                    buffer
                }
                Err(prompt::StdinError::TooLarge { limit }) => {
                    eprintln!(
                        "claude-print: stdin is larger than the {}-byte limit",
                        limit
                    );
                    exit_with_cleanup(2);
                }
                Err(prompt::StdinError::ReadFailed { source }) => {
                    eprintln!("claude-print: failed to read stdin: {}", source);
                    exit_with_cleanup(4);
                }
            }
        } else {
            // None found → exit 4
            eprintln!(
                "claude-print: no prompt provided (pass as argument, --input-file, or stdin)"
            );
            exit_with_cleanup(4);
        }
    };

    // EC-4: reject prompts containing an embedded NUL byte from any source
    // (positional, stdin, --input-file). `claude -p` does not support null
    // bytes, so this is a CLI validation failure → exit 2.
    if let Some(offset) = prompt::find_null_byte(&prompt_bytes) {
        eprintln!(
            "claude-print: prompt contains a null byte at offset {} (not supported)",
            offset
        );
        exit_with_cleanup(2);
    }

    // Load the config file once at startup (plan.md "Configuration File").
    // A missing file uses defaults; path resolution, read, parse, and validation
    // failures are hard errors (exit code 2, structured error in JSON modes).
    let config = match cli
        .config
        .clone()
        .map_or_else(Config::default_path, Ok)
        .and_then(|path| Config::load_or_default(&path))
    {
        Ok(config) => config,
        Err(e) => {
            let mut stdout = io::stdout().lock();
            let mut stderr = io::stderr().lock();
            let error: ClaudePrintError = e.into();
            let _ = emit_error(
                &mut stdout,
                &mut stderr,
                &error,
                &cli.output_format,
                &resolve_claude_version(cli.claude_binary.as_deref())
                    .unwrap_or_else(|| "unknown".to_string()),
                true,
            );
            exit_with_cleanup(error.exit_code());
        }
    };

    // Build claude_args: collect flags to forward to child
    let mut claude_args: Vec<std::ffi::OsString> = Vec::new();

    // Model precedence (plan.md "Model precedence"): CLI --model flag >
    // config.toml defaults.model > compiled-in default (claude-sonnet-4-6).
    // Always forward an explicit --model so the compiled-in default is applied
    // rather than letting the child silently fall back to its own internal
    // default when neither a flag nor a config value is present.
    for arg in build_model_args(&config, cli.model.clone()) {
        claude_args.push(arg.into());
    }

    // Max-turns precedence: CLI --max-turns flag > config.toml defaults.max_turns > 30.
    // Always forward an explicit --max-turns so the compiled-in default is applied
    // rather than letting the child silently fall back to its own internal default.
    for arg in build_max_turns_args(&config, Some(cli.max_turns)) {
        claude_args.push(arg.into());
    }

    // `--timeout` is claude-print's OWN wall-clock watchdog and is deliberately
    // NOT forwarded to the child: `claude` has never accepted a `--timeout`
    // option. Forwarding it made every invocation die at the child's argv
    // parsing with `error: unknown option '--timeout'` before the prompt was
    // ever injected, which broke claude-print completely against claude
    // 2.1.263. The resolved value is enforced locally by the watchdog below.
    //
    // Timeout precedence is unchanged: CLI --timeout > config.toml
    // defaults.timeout_secs > compiled-in default (3600).
    let resolved_timeout = config.resolve_timeout_secs(Some(cli.timeout));

    if cli.dangerously_skip_permissions {
        claude_args.push("--dangerously-skip-permissions".into());
    }

    if let Some(ref tools) = cli.allowed_tools {
        claude_args.push("--allowedTools".into());
        claude_args.push(tools.as_str().into());
    }

    if let Some(ref tools) = cli.disallowed_tools {
        claude_args.push("--disallowedTools".into());
        claude_args.push(tools.as_str().into());
    }

    let t0 = Instant::now();
    let output_format = cli.output_format; // Save before move

    // Launch options for the child: hook-inheritance mode plus the bf-uj0
    // headless-launch safety knobs (pre-trust cwd, bound MCP init, child-stderr
    // surfacing on slow/stall). All default off. The no_inherit_hooks flag is
    // consumed here — session.rs is the single source of truth that decides
    // whether `--setting-sources=` is forwarded to the child (Hard Requirement 5).
    // Resolve inherit_hooks: CLI --no-inherit-hooks flag > config defaults.inherit_hooks > true
    let resolved_no_inherit_hooks = !config.resolve_inherit_hooks(if cli.no_inherit_hooks {
        Some(false)
    } else {
        None
    });
    let launch = session::LaunchOptions {
        no_inherit_hooks: resolved_no_inherit_hooks,
        mcp_configs: cli.mcp_config.clone(),
        pretrust_cwd: cli.pretrust_cwd,
        show_child_stderr: cli.show_child_stderr,
        verbose: cli.verbose,
    };

    // Call session::Session::run()
    let result = session::Session::run(
        &claude_bin,
        &claude_args,
        prompt_bytes,
        Some(resolved_timeout),
        Some(cli.first_output_timeout),
        Some(cli.stream_json_timeout),
        Some(cli.stop_hook_timeout),
        output_format,
        &launch,
    );

    // Lock stdout and stderr for output
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();

    // Match result
    match result {
        Ok(session_result) => {
            let duration_ms = t0.elapsed().as_millis() as u64;

            // For stream-json format, the reader thread has already streamed all output.
            // For text and json formats, emit the success result.
            if output_format != claude_print::cli::OutputFormat::StreamJson {
                if let Err(e) = emitter::emit_success(
                    &mut stdout,
                    &session_result.transcript,
                    &cli.output_format,
                    &session_result.claude_version,
                    duration_ms,
                ) {
                    eprintln!("claude-print: failed to write output: {}", e);
                    exit_with_cleanup(2);
                }
            }
            exit_with_cleanup(0);
        }
        Err(Error::Interrupted(_msg)) => {
            let _ = emit_error(
                &mut stdout,
                &mut stderr,
                &ClaudePrintError::Interrupted,
                &cli.output_format,
                &resolve_claude_version(cli.claude_binary.as_deref())
                    .unwrap_or_else(|| "unknown".to_string()),
                true,
            );
            exit_with_cleanup(130);
        }
        Err(Error::Timeout(_msg)) => {
            let _ = emit_error(
                &mut stdout,
                &mut stderr,
                &ClaudePrintError::Timeout,
                &cli.output_format,
                &resolve_claude_version(cli.claude_binary.as_deref())
                    .unwrap_or_else(|| "unknown".to_string()),
                true,
            );
            exit_with_cleanup(ClaudePrintError::Timeout.exit_code());
        }
        Err(Error::Internal(e)) => {
            let msg = if e
                .to_string()
                .contains("Child exited without sending Stop payload")
            {
                "claude exited before Stop hook fired".to_string()
            } else {
                e.to_string()
            };
            let _ = emit_error(
                &mut stdout,
                &mut stderr,
                &ClaudePrintError::Setup(msg),
                &cli.output_format,
                &resolve_claude_version(cli.claude_binary.as_deref())
                    .unwrap_or_else(|| "unknown".to_string()),
                true,
            );
            exit_with_cleanup(2);
        }
        Err(Error::AssistantError(msg)) => {
            // bf-416c: the turn completed but Claude Code's own transcript
            // reported is_error:true. Exit 1 (not 2) and emit an error result
            // so callers that gate on exit code / is_error don't treat a failed
            // turn as success. The prompt was injected, so stream-json after-inject
            // writes the synthesized error JSON to stdout.
            let err = ClaudePrintError::AssistantError(msg);
            let _ = emit_error(
                &mut stdout,
                &mut stderr,
                &err,
                &cli.output_format,
                &resolve_claude_version(cli.claude_binary.as_deref())
                    .unwrap_or_else(|| "unknown".to_string()),
                true,
            );
            exit_with_cleanup(err.exit_code());
        }
        Err(e) => {
            let _ = emit_error(
                &mut stdout,
                &mut stderr,
                &ClaudePrintError::Setup(e.to_string()),
                &cli.output_format,
                &resolve_claude_version(cli.claude_binary.as_deref())
                    .unwrap_or_else(|| "unknown".to_string()),
                true,
            );
            exit_with_cleanup(2);
        }
    }
}

/// Emit an error in the appropriate format.
fn emit_error(
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    error: &ClaudePrintError,
    format: &claude_print::cli::OutputFormat,
    claude_version: &str,
    stream_json_after_inject: bool,
) -> std::io::Result<()> {
    emitter::emit_error(
        stdout,
        stderr,
        error,
        format,
        claude_version,
        stream_json_after_inject,
    )
}

/// Build the `--model <resolved>` argv pair to forward to the child, applying
/// the plan's documented precedence: CLI `--model` flag > `config.toml`
/// `defaults.model` > compiled-in default (`claude-sonnet-4-6`).
///
/// Always returns a two-element pair (never empty) so the model forwarded to
/// the child is always explicit — the compiled-in default is applied here
/// rather than being left to the child's own internal default.
fn build_model_args(config: &Config, cli_model: Option<String>) -> Vec<String> {
    vec!["--model".to_string(), config.resolve_model(cli_model)]
}

/// Build the `--max-turns <resolved>` argv pair to forward to the child, applying
/// the plan's documented precedence: CLI `--max-turns` flag > `config.toml`
/// `defaults.max_turns` > compiled-in default (30).
///
/// Always returns a two-element pair (never empty) so the max-turns forwarded to
/// the child is always explicit — the compiled-in default is applied here
/// rather than being left to the child's own internal default.
fn build_max_turns_args(config: &Config, cli_max_turns: Option<u32>) -> Vec<String> {
    vec![
        "--max-turns".to_string(),
        config.resolve_max_turns(cli_max_turns).to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &std::path::Path, model: Option<&str>) -> std::path::PathBuf {
        let path = dir.join("claude-print.toml");
        let contents = match model {
            Some(m) => format!("[defaults]\nmodel = \"{m}\"\n"),
            None => String::new(),
        };
        std::fs::write(&path, contents).unwrap();
        path
    }

    // (a) no --model flag + no config file → compiled-in default forwarded.
    #[test]
    fn model_args_compiled_default_when_no_flag_and_no_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load_or_default(&dir.path().join("does-not-exist.toml")).unwrap();
        let args = build_model_args(&config, None);
        assert_eq!(
            args,
            vec!["--model".to_string(), "claude-sonnet-4-6".to_string()]
        );
    }

    // (b) no --model flag + config sets a model → config model forwarded.
    #[test]
    fn model_args_config_model_when_no_flag() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), Some("claude-opus-4-8"));
        let config = Config::load_or_default(&path).unwrap();
        let args = build_model_args(&config, None);
        assert_eq!(
            args,
            vec!["--model".to_string(), "claude-opus-4-8".to_string()]
        );
    }

    // (c) --model flag set + config also sets a model → CLI flag wins.
    #[test]
    fn model_args_cli_flag_wins_over_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), Some("claude-opus-4-8"));
        let config = Config::load_or_default(&path).unwrap();
        let args = build_model_args(&config, Some("claude-haiku-4-5".to_string()));
        assert_eq!(
            args,
            vec!["--model".to_string(), "claude-haiku-4-5".to_string()]
        );
    }

    // Helper to write config with max_turns
    fn write_config_max_turns(dir: &std::path::Path, max_turns: Option<u32>) -> std::path::PathBuf {
        let path = dir.join("claude-print.toml");
        let contents = match max_turns {
            Some(m) => format!("[defaults]\nmax_turns = {m}\n"),
            None => String::new(),
        };
        std::fs::write(&path, contents).unwrap();
        path
    }

    // Helper to write config with timeout
    fn write_config_timeout(dir: &std::path::Path, timeout: Option<u64>) -> std::path::PathBuf {
        let path = dir.join("claude-print.toml");
        let contents = match timeout {
            Some(t) => format!("[defaults]\ntimeout_secs = {t}\n"),
            None => String::new(),
        };
        std::fs::write(&path, contents).unwrap();
        path
    }

    // (a) no --max-turns flag + no config file → compiled-in default (30) forwarded.
    #[test]
    fn max_turns_args_compiled_default_when_no_flag_and_no_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load_or_default(&dir.path().join("does-not-exist.toml")).unwrap();
        let args = build_max_turns_args(&config, None);
        assert_eq!(args, vec!["--max-turns".to_string(), "30".to_string()]);
    }

    // (b) no --max-turns flag + config sets max_turns → config value forwarded.
    #[test]
    fn max_turns_args_config_value_when_no_flag() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config_max_turns(dir.path(), Some(50));
        let config = Config::load_or_default(&path).unwrap();
        let args = build_max_turns_args(&config, None);
        assert_eq!(args, vec!["--max-turns".to_string(), "50".to_string()]);
    }

    // (c) --max-turns 30 flag + config sets max_turns → explicit 30 wins over config.
    #[test]
    fn max_turns_args_explicit_30_overrides_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config_max_turns(dir.path(), Some(50));
        let config = Config::load_or_default(&path).unwrap();
        let args = build_max_turns_args(&config, Some(30));
        assert_eq!(args, vec!["--max-turns".to_string(), "30".to_string()]);
    }

    // (d) --max-turns 5 flag + config sets max_turns → CLI flag wins.
    #[test]
    fn max_turns_args_cli_flag_wins_over_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config_max_turns(dir.path(), Some(50));
        let config = Config::load_or_default(&path).unwrap();
        let args = build_max_turns_args(&config, Some(5));
        assert_eq!(args, vec!["--max-turns".to_string(), "5".to_string()]);
    }

    // Regression: `--timeout` is claude-print's own watchdog and must never be
    // forwarded to the child. `claude` has no such option, so forwarding it
    // killed every invocation at the child's argv parsing with
    // `error: unknown option '--timeout'` before the prompt was injected.
    // The child argv is built only from the builders exercised below plus the
    // pass-through permission/tool flags; none of them may emit `--timeout`.
    #[test]
    fn timeout_is_never_forwarded_to_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config_timeout(dir.path(), Some(1800));
        let config = Config::load_or_default(&path).unwrap();

        let mut forwarded: Vec<String> = Vec::new();
        forwarded.extend(build_model_args(&config, None));
        forwarded.extend(build_max_turns_args(&config, Some(30)));

        assert!(
            !forwarded.iter().any(|a| a == "--timeout"),
            "--timeout must not reach the child; got {forwarded:?}"
        );
    }

    // The timeout still has to be *resolved* with the documented precedence —
    // it just feeds the local watchdog instead of the child argv.
    #[test]
    fn timeout_still_resolves_for_the_local_watchdog() {
        let dir = tempfile::tempdir().unwrap();

        // (a) no flag + no config -> compiled-in default.
        let config = Config::load_or_default(&dir.path().join("does-not-exist.toml")).unwrap();
        assert_eq!(config.resolve_timeout_secs(None), 3600);

        // (b) no flag + config value -> config wins.
        let path = write_config_timeout(dir.path(), Some(1800));
        let config = Config::load_or_default(&path).unwrap();
        assert_eq!(config.resolve_timeout_secs(None), 1800);

        // (c) explicit flag -> flag wins over config.
        assert_eq!(config.resolve_timeout_secs(Some(7200)), 7200);
    }
}
