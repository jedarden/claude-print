use clap::{Parser, Subcommand, ValueEnum};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, ValueEnum)]
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

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the pool daemon (ADR-005 warm PTY pool)
    #[command(name = "serve")]
    Serve {
        /// Number of workers to maintain in the pool
        #[arg(long, default_value = "1")]
        pool_size: usize,

        /// Socket path to listen on (default: /tmp/claude-print-pool.sock)
        #[arg(long)]
        socket: Option<String>,

        /// Verbose logging
        #[arg(long)]
        verbose: bool,
    },
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
    /// Subcommand (serve, etc.)
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Prompt string (mutually exclusive with --input-file and stdin)
    #[arg(value_name = "PROMPT", required = false)]
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

    /// First-output timeout in seconds (PTY output, default: 90)
    #[arg(long, default_value = "90")]
    pub first_output_timeout: u64,

    /// Stream-json first-output timeout in seconds (default: 90)
    #[arg(long, default_value = "90")]
    pub stream_json_timeout: u64,

    /// Stop hook watchdog timeout in seconds (default: 120)
    #[arg(long, default_value = "120")]
    pub stop_hook_timeout: u64,

    /// Path to claude binary (default: resolved from PATH)
    #[arg(long = "claude-binary")]
    pub claude_binary: Option<std::path::PathBuf>,

    /// Disable user hook inheritance
    #[arg(long = "no-inherit-hooks")]
    pub no_inherit_hooks: bool,

    /// MCP config (path or inline JSON) to load. Headless runs always pass
    /// `--strict-mcp-config` to the child so only configs named here are loaded
    /// — inherited/project/global MCP servers cannot wedge startup. May be
    /// repeated, or comma-separated for multiple files.
    #[arg(long = "mcp-config", value_delimiter = ',')]
    pub mcp_config: Vec<String>,

    /// Pre-grant folder trust for the working dir by writing
    /// `hasTrustDialogAccepted: true` into `~/.claude.json` before spawning the
    /// child. Claude Code reads trust only from that file (not from `--settings`),
    /// so this is the only way to prevent the one-time trust dialog from
    /// stalling an untrusted cwd without relying on the PTY keyword scanner.
    /// Off by default to avoid mutating the shared user config under fleet
    /// concurrency; enable it when you have seen trust-dialog stalls.
    #[arg(long = "pretrust-cwd")]
    pub pretrust_cwd: bool,

    /// Surface the child's captured PTY output to stderr when startup is slow
    /// or stalls (watchdog first-output timeout, or the prompt was never
    /// injected). The child runs under a PTY, so this is its combined
    /// stdout/stderr — useful for diagnosing MCP/init wedges.
    #[arg(long = "show-child-stderr")]
    pub show_child_stderr: bool,

    /// Write timing traces to stderr
    #[arg(long)]
    pub verbose: bool,

    /// Run installation self-test and exit
    #[arg(long)]
    pub check: bool,

    /// Remove orphaned temp directories found by --check
    #[arg(long, requires = "check")]
    pub clean: bool,

    /// Print version and exit
    #[arg(long = "version", short = 'V')]
    pub version: bool,

    /// Connect to a pool daemon at this socket path (ADR-005 warm PTY pool)
    #[arg(long = "pool-socket")]
    pub pool_socket: Option<std::path::PathBuf>,

    /// Path to the config file (default: `$XDG_CONFIG_HOME/claude-print/config.toml`
    /// or `$HOME/.config/claude-print/config.toml`). The CLI requires a valid
    /// `HOME` even when this option supplies an explicit path; see
    /// [`get_home`](crate::util::get_home) for the canonical strict policy.
    #[arg(long = "config")]
    pub config: Option<std::path::PathBuf>,
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
