use clap::{Parser, ValueEnum};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
    #[value(name = "stream-json")]
    StreamJson,
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputFormat::Text => write!(f, "text"),
            OutputFormat::Json => write!(f, "json"),
            OutputFormat::StreamJson => write!(f, "stream-json"),
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "claude-print",
    about = "Drop-in replacement for `claude -p` billing against the subscription pool",
    version = VERSION,
    long_version = VERSION,
    disable_version_flag = true,
)]
pub struct Cli {
    /// Prompt string (mutually exclusive with --input-file and stdin)
    pub prompt: Option<String>,

    /// Read prompt from file
    #[arg(long = "input-file", short = 'f')]
    pub input_file: Option<std::path::PathBuf>,

    /// Model to use (default: claude-sonnet-4-6)
    #[arg(long, short = 'm')]
    pub model: Option<String>,

    /// Maximum number of turns (default: 30)
    #[arg(long, default_value = "30")]
    pub max_turns: u32,

    /// Output format
    #[arg(long = "output-format", short = 'o', default_value = "text")]
    pub output_format: OutputFormat,

    /// Comma-separated list of allowed tools
    #[arg(long = "allowedTools")]
    pub allowed_tools: Option<String>,

    /// Comma-separated list of disallowed tools
    #[arg(long = "disallowedTools")]
    pub disallowed_tools: Option<String>,

    /// Skip permission prompts (dangerous)
    #[arg(long = "dangerously-skip-permissions")]
    pub dangerously_skip_permissions: bool,

    /// Wall-clock timeout in seconds (default: 3600)
    #[arg(long, default_value = "3600")]
    pub timeout: u64,

    /// Path to claude binary (default: resolved from PATH)
    #[arg(long = "claude-binary")]
    pub claude_binary: Option<std::path::PathBuf>,

    /// Disable user hook inheritance
    #[arg(long = "no-inherit-hooks")]
    pub no_inherit_hooks: bool,

    /// Write timing traces to stderr
    #[arg(long)]
    pub verbose: bool,

    /// Run installation self-test and exit
    #[arg(long)]
    pub check: bool,

    /// Print version and exit
    #[arg(long = "version", short = 'V')]
    pub version: bool,
}

pub fn version_string(claude_version: Option<&str>) -> String {
    let claude_part = match claude_version {
        Some(v) => v.to_string(),
        None => "not found".to_string(),
    };
    format!("claude-print {} (wrapping claude {})", VERSION, claude_part)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_string_with_claude_version() {
        let s = version_string(Some("2.1.3"));
        assert!(s.starts_with("claude-print "));
        assert!(s.contains("wrapping claude 2.1.3"));
    }

    #[test]
    fn version_string_without_claude() {
        let s = version_string(None);
        assert!(s.contains("not found"));
    }

    #[test]
    fn version_format_matches_expected_pattern() {
        let s = version_string(Some("2.0.0"));
        assert_eq!(
            s,
            format!("claude-print {} (wrapping claude 2.0.0)", VERSION)
        );
    }
}
