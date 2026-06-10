use clap::Parser;
use claude_print::cli::{version_string, Cli};
use std::process;

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

    eprintln!("claude-print: not yet implemented");
    process::exit(2);
}
