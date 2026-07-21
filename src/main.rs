use clap::Parser;
use claude_print::cli::{version_string, Cli};
use claude_print::config::Config;
use claude_print::emitter;
use claude_print::error::{ClaudePrintError, Error};
use claude_print::hook;
use claude_print::session;
use std::io::{self, Read, Write};
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

    // Clean up orphaned temp dirs from previous crashed runs.
    // This runs on all invocations, not just when a session runs,
    // ensuring orphans are eventually removed.
    hook::cleanup_orphans();

    let cli = Cli::parse();

    if cli.version {
        let claude_version = resolve_claude_version(cli.claude_binary.as_deref());
        println!("{}", version_string(claude_version.as_deref()));
        exit_with_cleanup(0);
    }

    if cli.check {
        let code = claude_print::check::run(cli.claude_binary.as_deref());
        exit_with_cleanup(code);
    }

    // Resolve the claude binary path
    let claude_bin = cli
        .claude_binary
        .clone()
        .unwrap_or_else(|| PathBuf::from("claude"));

    // AS-5: Check if claude binary exists before calling session::run()
    if which::which(&claude_bin).is_err() {
        eprintln!(
            "claude-print: '{}' not found in PATH",
            claude_bin.to_string_lossy()
        );
        exit_with_cleanup(2);
    }

    // Prompt resolution (in order of precedence)
    let prompt_bytes = if let Some(ref input_file) = cli.input_file {
        // --input-file <path>: read file bytes
        match std::fs::read(input_file) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!(
                    "claude-print: failed to read input file '{}': {}",
                    input_file.display(),
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
            let mut buffer = Vec::new();
            if let Err(e) = io::stdin().read_to_end(&mut buffer) {
                eprintln!("claude-print: failed to read stdin: {}", e);
                exit_with_cleanup(4);
            }
            if buffer.is_empty() {
                eprintln!(
                    "claude-print: no prompt provided (pass as argument, --input-file, or stdin)"
                );
                exit_with_cleanup(4);
            }
            buffer
        } else {
            // None found → exit 4
            eprintln!(
                "claude-print: no prompt provided (pass as argument, --input-file, or stdin)"
            );
            exit_with_cleanup(4);
        }
    };

    // Load the config file once at startup (plan.md "Configuration File"). A
    // missing $HOME or a missing/invalid file yields an empty config, so this
    // never aborts the run — it just means no config-derived defaults apply.
    let config = Config::default_path()
        .map(|path| Config::load_or_default(&path))
        .unwrap_or_default();

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

    if cli.max_turns != 30 {
        // Only pass if non-default
        claude_args.push("--max-turns".into());
        claude_args.push(cli.max_turns.to_string().into());
    }

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
    let launch = session::LaunchOptions {
        no_inherit_hooks: cli.no_inherit_hooks,
        mcp_configs: cli.mcp_config.clone(),
        pretrust_cwd: cli.pretrust_cwd,
        show_child_stderr: cli.show_child_stderr,
    };

    // Call session::Session::run()
    let result = session::Session::run(
        &claude_bin,
        &claude_args,
        prompt_bytes,
        Some(cli.timeout),
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
        let config = Config::load_or_default(&dir.path().join("does-not-exist.toml"));
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
        let config = Config::load_or_default(&path);
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
        let config = Config::load_or_default(&path);
        let args = build_model_args(&config, Some("claude-haiku-4-5".to_string()));
        assert_eq!(
            args,
            vec!["--model".to_string(), "claude-haiku-4-5".to_string()]
        );
    }
}
