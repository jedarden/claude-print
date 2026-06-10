use claude_print::event_loop::{EventLoop, ExitReason};
use claude_print::hook::HookInstaller;
use claude_print::poller::{open_fifo_nonblock, parse_stop_payload, resolve_stop_info};
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

    let info = resolve_stop_info(stop);
    assert_eq!(
        info.transcript_path,
        Some(std::path::PathBuf::from(
            "/tmp/test-transcript/test-session-123.jsonl"
        )),
        "StopInfo transcript_path should use the explicit payload path"
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

    let info = resolve_stop_info(stop);

    // Derived slug: /home/user/myproject → home-user-myproject
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let expected = std::path::PathBuf::from(&home)
        .join(".claude")
        .join("projects")
        .join("home-user-myproject")
        .join("abc123.jsonl");

    assert_eq!(
        info.transcript_path,
        Some(expected.clone()),
        "derived transcript_path should be {expected:?}"
    );
    assert_eq!(info.session_id.as_deref(), Some("abc123"));
}
