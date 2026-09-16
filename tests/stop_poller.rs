use claude_print::event_loop::{EventLoop, ExitReason};
use claude_print::hook::HookInstaller;
use claude_print::poller::{open_fifo_nonblock, parse_stop_payload, resolve_stop_info};
use claude_print::util::get_home;
use std::io::Write;
use std::os::unix::io::AsRawFd;

/// Verify that when a Stop JSON payload is written to the FIFO, the event loop
/// returns it via FifoPayload and parse_stop_payload extracts the fields.
#[test]
fn test_stop_hook_fires() {
    let installer = HookInstaller::new().expect("HookInstaller::new");

    // Open FIFO: keeper write-end + read-end (O_NONBLOCK, no ENXIO).
    let (fifo_read, _fifo_keeper) =
        open_fifo_nonblock(&installer.fifo_path).expect("open_fifo_nonblock");

    // Dummy "master" pipe — won't produce PTY data, so POLLIN won't fire on it.
    let (dummy_r, _dummy_w) = nix::unistd::pipe().expect("pipe");
    // Self-pipe for interrupt signalling — won't be written in this test.
    let (self_pipe_r, _self_pipe_w) = nix::unistd::pipe().expect("pipe");

    let mut el = EventLoop::new(dummy_r.as_raw_fd(), self_pipe_r.as_raw_fd());
    el.add_fifo_fd(fifo_read.as_raw_fd());

    // Simulate the Stop hook writing a JSON payload to the FIFO.
    let fifo_path = installer.fifo_path.clone();
    let payload_json = concat!(
        r#"{"hook_event_name":"Stop","session_id":"test-session-123","#,
        r#""transcript_path":"/tmp/test-transcript/test-session-123.jsonl","#,
        r#""cwd":"/tmp/test-cwd","last_assistant_message":"hello world"}"#,
    );
    let payload_bytes = payload_json.as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        // Blocking open; succeeds immediately because read-end (keeper) is open.
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&fifo_path)
            .expect("open FIFO for writing");
        f.write_all(&payload_bytes).expect("write payload");
        f.write_all(b"\n").expect("write newline");
    });

    let reason = el.run(|_| {}).expect("event loop");
    writer.join().expect("writer thread");

    let raw = match reason {
        ExitReason::FifoPayload(bytes) => bytes,
        other => panic!("expected FifoPayload, got {other:?}"),
    };

    let stop = parse_stop_payload(&raw).expect("parse_stop_payload");

    assert_eq!(
        stop.session_id.as_deref(),
        Some("test-session-123"),
        "session_id mismatch"
    );
    assert_eq!(
        stop.transcript_path.as_deref(),
        Some("/tmp/test-transcript/test-session-123.jsonl"),
        "transcript_path mismatch"
    );
    assert_eq!(
        stop.last_assistant_message.as_deref(),
        Some("hello world"),
        "last_assistant_message mismatch"
    );

    let info = resolve_stop_info(stop).expect("resolve_stop_info");
    assert_eq!(
        info.transcript_path,
        Some(std::path::PathBuf::from(
            "/tmp/test-transcript/test-session-123.jsonl"
        )),
        "StopInfo transcript_path should use the explicit payload path"
    );
}

/// Degraded-run Stop duplication (claudepr-8dcf53ce): permission-denied tool
/// runs can make Claude Code fire Stop more than once
/// (docs/notes/hook-design.md "Stop Firing Frequency"). The single-fire poller
/// acts on the FIRST payload; later firings must neither corrupt it nor
/// produce a second result.
///
/// Three hook invocations — three separate write-end opens, exactly how extra
/// `cat > fifo` hook firings land — are written BEFORE the poller runs, so the
/// first POLLIN wake-up observes all three payloads coalesced in the FIFO
/// buffer (the deterministic shape of a degraded run whose extra firings won
/// the race to the first read). The event loop must return exactly ONE
/// FifoPayload, and parsing it must yield the first payload's fields with the
/// duplicate and spurious lines ignored — not concatenated, not overwritten.
#[test]
fn test_extra_stop_firings_first_payload_wins_single_fire() {
    let installer = HookInstaller::new().expect("HookInstaller::new");

    let (fifo_read, _fifo_keeper) =
        open_fifo_nonblock(&installer.fifo_path).expect("open_fifo_nonblock");

    let (dummy_r, _dummy_w) = nix::unistd::pipe().expect("pipe");
    let (self_pipe_r, _self_pipe_w) = nix::unistd::pipe().expect("pipe");

    let mut el = EventLoop::new(dummy_r.as_raw_fd(), self_pipe_r.as_raw_fd());
    el.add_fifo_fd(fifo_read.as_raw_fd());

    let complete = concat!(
        r#"{"hook_event_name":"Stop","session_id":"test-session-123","#,
        r#""transcript_path":"/tmp/test-transcript/test-session-123.jsonl","#,
        r#""cwd":"/tmp/test-cwd","last_assistant_message":"hello world"}
"#,
    );
    let spurious = concat!(
        r#"{"hook_event_name":"Stop","session_id":"spurious-session-999","#,
        r#""transcript_path":"/tmp/test-transcript/spurious-session-999.jsonl","#,
        r#""cwd":"/tmp/test-cwd","last_assistant_message":"spurious extra stop"}
"#,
    );

    // Writes cannot block: the read-end and keeper write-end are open above,
    // and the three payloads total well under the pipe buffer, so all bytes
    // are sitting in the FIFO before the first poll() call.
    for payload in [complete, complete, spurious] {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&installer.fifo_path)
            .expect("open FIFO for writing");
        f.write_all(payload.as_bytes()).expect("write payload");
    }

    let reason = el.run(|_| {}).expect("event loop");

    let raw = match reason {
        ExitReason::FifoPayload(bytes) => bytes,
        other => panic!("expected FifoPayload, got {other:?}"),
    };

    // Self-check: the later firings really were delivered through the FIFO —
    // the buffer holds all three payloads. Without this, a regression that
    // silently dropped duplicate writes would pass vacuously.
    let text = std::str::from_utf8(&raw).expect("raw payload UTF-8");
    let non_empty_lines = text.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(
        non_empty_lines, 3,
        "all three Stop payloads must be in the read buffer, got: {text:?}"
    );
    assert!(
        text.contains("spurious-session-999"),
        "spurious later firing must be present in the buffer, got: {text:?}"
    );

    // Single fire, first payload wins: parse extracts ONLY the first line's
    // fields. A last-line-wins regression picks up spurious-session-999; a
    // concatenating regression fails JSON parse outright.
    let stop = parse_stop_payload(&raw).expect("parse_stop_payload");
    assert_eq!(
        stop.session_id.as_deref(),
        Some("test-session-123"),
        "first payload's session_id must win over later firings"
    );
    assert_eq!(
        stop.transcript_path.as_deref(),
        Some("/tmp/test-transcript/test-session-123.jsonl"),
        "first payload's transcript_path must win over later firings"
    );
    assert_eq!(
        stop.last_assistant_message.as_deref(),
        Some("hello world"),
        "first payload's last_assistant_message must win, not doubled or spurious"
    );

    let info = resolve_stop_info(stop).expect("resolve_stop_info");
    assert_eq!(
        info.transcript_path,
        Some(std::path::PathBuf::from(
            "/tmp/test-transcript/test-session-123.jsonl"
        )),
        "resolved transcript path must come from the first payload"
    );
}

/// When `transcript_path` is absent from the Stop payload, the transcript path
/// is derived from `session_id` + `cwd` using the documented slug algorithm.
#[test]
fn test_missing_transcript_path_derived() {
    let installer = HookInstaller::new().expect("HookInstaller::new");

    let (fifo_read, _fifo_keeper) =
        open_fifo_nonblock(&installer.fifo_path).expect("open_fifo_nonblock");

    let (dummy_r, _dummy_w) = nix::unistd::pipe().expect("pipe");
    let (self_pipe_r, _self_pipe_w) = nix::unistd::pipe().expect("pipe");

    let mut el = EventLoop::new(dummy_r.as_raw_fd(), self_pipe_r.as_raw_fd());
    el.add_fifo_fd(fifo_read.as_raw_fd());

    // Payload deliberately omits `transcript_path`.
    let fifo_path = installer.fifo_path.clone();
    let writer = std::thread::spawn(move || {
        let payload = concat!(
            r#"{"hook_event_name":"Stop","session_id":"abc123","#,
            r#""cwd":"/home/user/myproject","last_assistant_message":"derived test"}"#,
        );
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&fifo_path)
            .expect("open FIFO for writing");
        f.write_all(payload.as_bytes()).expect("write payload");
        f.write_all(b"\n").expect("write newline");
    });

    let reason = el.run(|_| {}).expect("event loop");
    writer.join().expect("writer thread");

    let raw = match reason {
        ExitReason::FifoPayload(bytes) => bytes,
        other => panic!("expected FifoPayload, got {other:?}"),
    };

    let stop = parse_stop_payload(&raw).expect("parse_stop_payload");

    // Confirm transcript_path is absent from the raw payload.
    assert!(
        stop.transcript_path.is_none(),
        "transcript_path should be absent from payload"
    );
    assert_eq!(stop.session_id.as_deref(), Some("abc123"));

    let info = resolve_stop_info(stop).expect("resolve_stop_info");

    // Derived slug: /home/user/myproject → -home-user-myproject. claude 2.1.263
    // folds every byte outside [a-zA-Z0-9] to '-', including the leading '/' —
    // the scheme poller::cwd_to_slug implements. HOME is resolved through the
    // production contract: an unset HOME is an error, never a /root fallback.
    let home = get_home().expect("test requires HOME");
    let expected = home
        .join(".claude")
        .join("projects")
        .join("-home-user-myproject")
        .join("abc123.jsonl");

    assert_eq!(
        info.transcript_path,
        Some(expected.clone()),
        "derived transcript_path should be {expected:?}"
    );
    assert_eq!(info.session_id.as_deref(), Some("abc123"));
}
