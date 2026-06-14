use clap::Parser;
use claude_print::cli::{version_string, Cli};
use claude_print::emitter;
use claude_print::error::{ClaudePrintError, Error};
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

fn main() {
    let cli = Cli::parse();

    if cli.version {
        let claude_version = resolve_claude_version(cli.claude_binary.as_deref());
        println!("{}", version_string(claude_version.as_deref()));
        process::exit(0);
    }

    if cli.check {
        let code = claude_print::check::run();
        process::exit(code);
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
        process::exit(2);
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
                process::exit(4);
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
                process::exit(4);
            }
            if buffer.is_empty() {
                eprintln!("claude-print: no prompt provided (pass as argument, --input-file, or stdin)");
                process::exit(4);
            }
            buffer
        } else {
            // None found → exit 4
            eprintln!("claude-print: no prompt provided (pass as argument, --input-file, or stdin)");
            process::exit(4);
        }
    };

    // Build claude_args: collect flags to forward to child
    let mut claude_args: Vec<std::ffi::OsString> = Vec::new();

    if let Some(ref model) = cli.model {
        claude_args.push("--model".into());
        claude_args.push(model.as_str().into());
    }

    if cli.max_turns != 30 {
        // Only pass if non-default
        claude_args.push("--max-turns".into());
        claude_args.push(cli.max_turns.to_string().into());
    }

    if cli.no_inherit_hooks {
        claude_args.push("--setting-sources=".into());
    }

    let t0 = Instant::now();

    // Call session::Session::run()
    let result = session::Session::run(&claude_bin, &claude_args, prompt_bytes, Some(cli.timeout));

    // Lock stdout and stderr for output
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();

    // Match result
    match result {
        Ok(session_result) => {
            let duration_ms = t0.elapsed().as_millis() as u64;

            // For stream-json format, replay the transcript line by line
            if cli.output_format == claude_print::cli::OutputFormat::StreamJson {
                if let Err(e) = replay_stream_json(&session_result.transcript_path) {
                    let _ = emit_error(
                        &mut stdout,
                        &mut stderr,
                        &ClaudePrintError::Setup(format!(
                            "failed to replay transcript: {}",
                            e
                        )),
                        &cli.output_format,
                        &session_result.claude_version,
                        false,
                    );
                    process::exit(2);
                }
            } else {
                // For text and json formats, emit success
                if let Err(e) = emitter::emit_success(
                    &mut stdout,
                    &session_result.transcript,
                    &cli.output_format,
                    &session_result.claude_version,
                    duration_ms,
                ) {
                    eprintln!("claude-print: failed to write output: {}", e);
                    process::exit(2);
                }
            }
            process::exit(0);
        }
        Err(Error::Interrupted(_msg)) => {
            let _ = emit_error(
                &mut stdout,
                &mut stderr,
                &ClaudePrintError::Interrupted,
                &cli.output_format,
                &resolve_claude_version(cli.claude_binary.as_deref()).unwrap_or_else(|| "unknown".to_string()),
                true,
            );
            process::exit(130);
        }
        Err(Error::Timeout(msg)) => {
            let _ = emit_error(
                &mut stdout,
                &mut stderr,
                &ClaudePrintError::Timeout,
                &cli.output_format,
                &resolve_claude_version(cli.claude_binary.as_deref()).unwrap_or_else(|| "unknown".to_string()),
                true,
            );
            process::exit(3);
        }
        Err(Error::Internal(e)) => {
            let msg = if e.to_string().contains("Child exited without sending Stop payload") {
                "claude exited before Stop hook fired".to_string()
            } else {
                e.to_string()
            };
            let _ = emit_error(
                &mut stdout,
                &mut stderr,
                &ClaudePrintError::Setup(msg),
                &cli.output_format,
                &resolve_claude_version(cli.claude_binary.as_deref()).unwrap_or_else(|| "unknown".to_string()),
                true,
            );
            process::exit(2);
        }
        Err(e) => {
            let _ = emit_error(
                &mut stdout,
                &mut stderr,
                &ClaudePrintError::Setup(e.to_string()),
                &cli.output_format,
                &resolve_claude_version(cli.claude_binary.as_deref()).unwrap_or_else(|| "unknown".to_string()),
                true,
            );
            process::exit(2);
        }
    }
}

/// Replay the transcript as stream-json output.
fn replay_stream_json(transcript_path: &std::path::Path) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(transcript_path)?;
    let reader = BufReader::new(file);
    let mut stdout = io::stdout().lock();

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            writeln!(stdout, "{}", trimmed)?;
        }
    }
    Ok(())
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
    emitter::emit_error(stdout, stderr, error, format, claude_version, stream_json_after_inject)
}
