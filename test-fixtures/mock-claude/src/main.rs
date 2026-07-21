use std::io::Write;
use std::thread;
use std::time::Duration;

fn main() {
    // Test fixture: when MOCK_RECORD_ARGS points at a path, dump the full argv
    // this process received (NUL-separated, mirroring /proc/<pid>/cmdline) so
    // integration tests can assert on the exact flags claude-print forwarded to
    // the child — e.g. whether `--setting-sources=` is present. Written FIRST so
    // it fires even under MOCK_SILENT / MOCK_EXIT_BEFORE_STOP.
    //
    // SKIPPED for the `--version` probe: claude-print resolves the child version
    // by running `<bin> --version` both before spawning the session child AND
    // again on the error-exit path (main.rs re-resolves it to render the error
    // result). Recording a `--version` call would overwrite the real child's
    // argv with just `[mock, --version]`, so the only argv we ever record is the
    // actual session child's. mock_claude is test-only, so MOCK_RECORD_ARGS is
    // never set in production.
    let is_version_probe = std::env::args().nth(1).as_deref() == Some("--version");
    if !is_version_probe {
        if let Ok(path) = std::env::var("MOCK_RECORD_ARGS") {
            let mut bytes: Vec<u8> = Vec::new();
            for arg in std::env::args() {
                bytes.extend_from_slice(arg.as_bytes());
                bytes.push(0);
            }
            let _ = std::fs::write(&path, &bytes);
        }
    }

    // Discover the stop FIFO path. mock-claude simulates the Stop hook that real
    // claude fires — it does not execute hooks, so it writes the Stop payload to
    // the fifo directly. claude-print never passes the fifo path in the child
    // argv; instead derive it from `--settings=<dir>/settings.json` (the fifo is
    // the `stop.fifo` sibling in the same temp dir — exactly the relationship the
    // installed hook.sh relies on, since its fifo path is baked in at install
    // time from this same dir). Falls back to positional arg 1 for the legacy
    // direct-spawn mode (test_pty_spawns_tty), which passes the fifo as its sole
    // arg with no --settings. NB: `--setting-sources=` does NOT match the
    // `--settings=` prefix (the next char is `-`, not `s`), so isolation mode is
    // unaffected.
    let fifo_path: Option<String> = std::env::args()
        .find_map(|a| {
            a.strip_prefix("--settings=").map(|s| {
                std::path::Path::new(s)
                    .parent()
                    .map(|d| d.join("stop.fifo").to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
        })
        .or_else(|| std::env::args().nth(1));

    // ── Env var controls ──────────────────────────────────────────────────────
    let mock_silent = env_flag("MOCK_SILENT");
    let mock_exit_before_stop = env_flag("MOCK_EXIT_BEFORE_STOP");
    let mock_delay_stop_ms: u64 = env_u64("MOCK_DELAY_STOP", 0);
    let mock_trust_dialog = env_flag("MOCK_TRUST_DIALOG");
    let mock_trust_wording = std::env::var("MOCK_TRUST_WORDING").unwrap_or_default();
    let mock_unknown_probe = env_flag("MOCK_UNKNOWN_PROBE");
    let mock_response =
        std::env::var("MOCK_RESPONSE").unwrap_or_else(|_| "Hello from mock_claude".to_string());
    let omit_transcript_path = env_flag("MOCK_OMIT_TRANSCRIPT_PATH");
    let omit_last_message = env_flag("MOCK_OMIT_LAST_MESSAGE");
    // bf-3isy: make mock_claude honor the transcript_path it reports. Real
    // claude writes a transcript JSONL file at this path; until now mock_claude
    // only sent `last_assistant_message` inline over the Stop FIFO, so the retry
    // loop in src/transcript.rs always exhausted and fell back — leaving AS-6
    // (Stop-before-JSONL race) with zero coverage and the stream-json live
    // reader (bf-5vm) with nothing to tail.
    //
    // MOCK_DELAY_JSONL=<ms>: write the transcript file <ms> AFTER the Stop FIFO
    // payload is sent, simulating the race where Claude fires Stop before
    // flushing the transcript — exactly what read_transcript's 40×50ms retry
    // loop exists to absorb.
    // MOCK_IS_ERROR=1: stamp the result event with is_error:true (maps to
    // Session::run's exit-1 AssistantError path).
    //
    // MOCK_STOP_BEFORE_INJECT=1 (bf-3i07): fire the Stop hook immediately, with
    // NO trust-dialog output and NO delay — before claude-print's startup
    // scanner can reach PROMPT_INJECTED. This is the EC-7 fixture: a response to
    // a prompt that was never sent. Real Claude Code is prevented from this by
    // EC-11 (pty.rs unsets CLAUDE_CODE_SESSION_ID before execvp); this fixture
    // simulates the leak that the EC-7 backstop in session.rs guards against.
    //
    // OUT OF SCOPE (follow-up): MOCK_TURNS, MOCK_UNKNOWN_EVENT_TYPE,
    // MOCK_UNKNOWN_USAGE_FIELDS — the plan's full MOCK_* matrix is intentionally
    // not implemented here; this bead only adds the minimum (file write + delay
    // + is_error) to unblock AS-6.
    let mock_delay_jsonl_ms: u64 = env_u64("MOCK_DELAY_JSONL", 0);
    let mock_is_error = env_flag("MOCK_IS_ERROR");
    let mock_stop_before_inject = env_flag("MOCK_STOP_BEFORE_INJECT");

    // Handle --version before MOCK_SILENT so version resolution works in tests
    // This is needed because Session::run() resolves the version before spawning
    // the PTY child, and we need the timeout path to work correctly.
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "--version" {
        println!("mock-claude-version-1.0.0");
        std::process::exit(0);
    }

    // MOCK_SILENT: block forever without firing Stop (tests timeout path)
    if mock_silent {
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }

    // Optionally emit an unknown ESC sequence (tests unknown-probe resilience)
    if mock_unknown_probe {
        print!("\x1b[999t");
        std::io::stdout().flush().ok();
    }

    // Optionally emit trust dialog text. Suppressed under MOCK_STOP_BEFORE_INJECT
    // so the Stop hook (fired below) wins the race against claude-print's startup
    // scanner — emitting trust keywords here would let the scanner reach
    // PROMPT_INJECTED and turn the run into a normal success instead of the EC-7
    // leak this fixture is meant to produce.
    if mock_trust_dialog && !mock_stop_before_inject {
        if mock_trust_wording == "alternate" {
            // Uses "continue" + "folder" as trust keywords
            print!("Do you want to continue and grant permission to this folder?\r\n");
        } else {
            // Standard wording uses "trust" + "Allow"
            print!("Do you trust and Allow access to this folder?\r\n");
        }
        std::io::stdout().flush().ok();
    }

    // MOCK_EXIT_BEFORE_STOP: exit without writing to the FIFO (tests child-exit-before-Stop)
    if mock_exit_before_stop {
        std::process::exit(1);
    }

    // Delay Stop if requested. Skipped under MOCK_STOP_BEFORE_INJECT so the Stop
    // fires immediately (see comment above the trust-dialog emission).
    if mock_delay_stop_ms > 0 && !mock_stop_before_inject {
        thread::sleep(Duration::from_millis(mock_delay_stop_ms));
    }

    let Some(fifo_path) = fifo_path else {
        // No FIFO path provided — exit cleanly (used when invoked without args)
        std::process::exit(0);
    };

    let session_id = "mock-session-abc123";
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/tmp".to_string());
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());

    let last_msg_part = if omit_last_message {
        String::new()
    } else {
        format!(
            ",\"last_assistant_message\":\"{}\"",
            json_escape(&mock_response)
        )
    };

    // transcript_path reported in the Stop payload. Held as a String so the
    // same path is both advertised to claude-print AND written below (the
    // contract real claude honors). None when the caller asked to omit it.
    let transcript_path: Option<String> = if omit_transcript_path {
        None
    } else {
        Some(format!(
            "{home}/.claude/projects/mock-cwd/{session_id}.jsonl"
        ))
    };

    let transcript_path_part = match &transcript_path {
        Some(p) => format!(",\"transcript_path\":\"{}\"", json_escape(p)),
        None => String::new(),
    };

    let payload = format!(
        "{{\"hook_event_name\":\"Stop\",\"session_id\":\"{session_id}\"{transcript_path_part},\"cwd\":\"{cwd}\"{last_msg_part}}}\n"
    );

    // O_WRONLY on a FIFO blocks until a reader opens the other end.
    if let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(&fifo_path) {
        let _ = file.write_all(payload.as_bytes());
    }

    // bf-3isy: write the transcript JSONL to the path advertised above. With
    // MOCK_DELAY_JSONL the write lands <ms> AFTER the Stop payload, so the
    // Stop-before-JSONL race window is real (the retry loop must absorb it).
    // Skipped when transcript_path was omitted — there is no advertised path
    // to honor, and MOCK_OMIT_TRANSCRIPT_PATH's own scenario relies on the
    // last_assistant_message fallback.
    if let Some(path) = transcript_path {
        if mock_delay_jsonl_ms > 0 {
            thread::sleep(Duration::from_millis(mock_delay_jsonl_ms));
        }
        write_transcript_jsonl(&path, &mock_response, session_id, mock_is_error);
    }

    // Exit 0 if stdin is a controlling TTY (login_tty succeeded), 1 otherwise.
    let has_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
    std::process::exit(if has_tty { 0 } else { 1 });
}

fn env_flag(key: &str) -> bool {
    std::env::var(key).map(|v| v == "1").unwrap_or(false)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Escape a string for safe embedding inside a JSON string literal.
///
/// Handles the characters that matter for MOCK_RESPONSE values: backslash,
/// double-quote, and the JSONL-breaking control chars (newline / CR / tab — a
/// raw newline would split one logical event across two lines and break
/// `transcript::parse_transcript`'s line-based reader).
fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Write a well-formed Claude Code transcript JSONL file at `path`.
///
/// Emits two lines — an `assistant` event (the turn text + a usage object
/// carrying all four token fields) followed by a `result` event — which is the
/// minimal shape `transcript::parse_transcript` needs to extract text, usage,
/// session_id, and is_error. Parent directories are created as needed. Errors
/// are swallowed: mock_claude is a test fixture, and a failed write simply
/// leaves the retry loop / last_assistant_message fallback as the source of
/// truth (matching pre-bf-3isy behavior).
fn write_transcript_jsonl(path: &str, response: &str, session_id: &str, is_error: bool) {
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // assistant event — message.id, content text=MOCK_RESPONSE, all 4 usage
    // token fields (non-zero so the happy-path "non-zero token counts"
    // assertion holds).
    let assistant = format!(
        "{{\"type\":\"assistant\",\"message\":{{\"id\":\"msg_mock_001\",\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}],\"usage\":{{\"input_tokens\":10,\"output_tokens\":25,\"cache_creation_input_tokens\":5,\"cache_read_input_tokens\":15}}}}}}",
        text = json_escape(response)
    );
    // result event — is_error reflects MOCK_IS_ERROR; session_id flows through
    // to the SessionResult so callers can assert on it.
    let result = format!(
        "{{\"type\":\"result\",\"session_id\":\"{session_id}\",\"is_error\":{is_error}}}",
        is_error = if is_error { "true" } else { "false" }
    );

    let _ = std::fs::write(path, format!("{assistant}\n{result}\n"));
}
