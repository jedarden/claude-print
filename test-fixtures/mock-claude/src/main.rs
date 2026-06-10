use std::io::Write;

fn main() {
    let fifo_path = std::env::args()
        .nth(1)
        .expect("usage: mock-claude <fifo-path>");

    let omit_transcript_path = std::env::var("MOCK_OMIT_TRANSCRIPT_PATH")
        .map(|v| v == "1")
        .unwrap_or(false);

    let session_id = "mock-session-abc123";
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/tmp".to_string());
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());

    // Build Stop hook JSON payload manually (no serde_json dep in mock-claude).
    // Paths on Linux cannot contain backslashes or control chars, so no escaping needed.
    let payload = if omit_transcript_path {
        format!(
            "{{\"hook_event_name\":\"Stop\",\"session_id\":\"{session_id}\",\"cwd\":\"{cwd}\",\"last_assistant_message\":\"Hello from mock_claude\"}}\n"
        )
    } else {
        format!(
            "{{\"hook_event_name\":\"Stop\",\"session_id\":\"{session_id}\",\"transcript_path\":\"{home}/.claude/projects/mock-cwd/{session_id}.jsonl\",\"cwd\":\"{cwd}\",\"last_assistant_message\":\"Hello from mock_claude\"}}\n"
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
