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

    // Optionally emit trust dialog text
    if mock_trust_dialog {
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

    // Delay Stop if requested
    if mock_delay_stop_ms > 0 {
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
            mock_response.replace('\\', "\\\\").replace('"', "\\\"")
        )
    };

    let payload = if omit_transcript_path {
        format!(
            "{{\"hook_event_name\":\"Stop\",\"session_id\":\"{session_id}\",\"cwd\":\"{cwd}\"{last_msg_part}}}\n"
        )
    } else {
        format!(
            "{{\"hook_event_name\":\"Stop\",\"session_id\":\"{session_id}\",\"transcript_path\":\"{home}/.claude/projects/mock-cwd/{session_id}.jsonl\",\"cwd\":\"{cwd}\"{last_msg_part}}}\n"
        )
    };

    // O_WRONLY on a FIFO blocks until a reader opens the other end.
    if let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(&fifo_path) {
        let _ = file.write_all(payload.as_bytes());
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
