//! Focused protocol-compatibility pins for the pool socket wire (ADR-005).
//!
//! The spec is [`docs/notes/pool-socket-protocol.md`]; its [Test map] section
//! points every documented claim at a test here or in the end-to-end suites.
//! Where the e2e suites (`pool_socket_e2e.rs`, `pool_failure_e2e.rs`,
//! `pool_adversarial_e2e.rs`) prove the protocol through real daemons driving
//! real `mock-claude` workers, this binary pins the wire contract itself, in
//! process, with no workers and no subprocesses:
//!
//!   * **wire frames** — the serde layer against literal JSON: tag and code
//!     registries, defaults, unknown-field tolerance, the legacy-assignment
//!     parse (the designed old-daemon detector), malformed bodies.
//!   * **client half** — the real `PoolClient` against hand-rolled fake
//!     daemons on real Unix sockets: happy path with a genuine SCM_RIGHTS fd
//!     transfer, every documented refusal code and its fallback
//!     classification, every malformed shape the spec catalogs, budget
//!     enforcement, the exact frames the client itself emits.
//!   * **daemon half** — a real `PoolServer` driven by a raw wire client:
//!     refusal frames (`pool_full`, `invalid_worker_id`, `shutting_down`),
//!     and the malformed-request stance (close, never reply) with the daemon
//!     staying healthy afterwards.
//!
//! Everything is hermetic: temp sockets, no `claude`/`mock-claude`, no
//! network, no fixed ports, no wall-clock dependencies beyond sub-second
//! poll ticks.
//!
//! [Test map]: docs/notes/pool-socket-protocol.md

use std::io::{ErrorKind, Read, Write};
use std::os::unix::io::{AsRawFd, IntoRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use claude_print::pool::{
    acquire_for_invocation, is_stateless_fallback, AcquireFailure, ErrorCode,
    InvocationAcquisition, PoolClient, PoolManager, PoolRequest, PoolResponse, PoolServer,
    DEFAULT_ACQUIRE_TIMEOUT_SECS,
};

/// The client budget used everywhere a fake daemon is expected to answer
/// promptly. Generous enough that a loaded CI machine cannot flake it, far
/// below the suite's per-test sanity ceilings.
const BUDGET: u64 = 5;

/// Budget for the deliberate-silence leg, where the assertion IS the deadline.
const SILENCE_BUDGET: u64 = 1;

/// Ceiling for any single test's wall clock. Nothing here should come near it;
/// it only converts a regression into a fast failure instead of a hang.
const TEST_CEILING: Duration = Duration::from_secs(15);

// ── Shared raw-wire helpers ──────────────────────────────────────────────────

fn temp_dir(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("poolproto-{tag}-"))
        .tempdir()
        .expect("temp dir")
}

/// Write one length-prefixed frame (the raw client half of the wire format).
fn send_frame(stream: &mut UnixStream, body: &[u8]) -> std::io::Result<()> {
    stream.write_all(&(body.len() as u32).to_be_bytes())?;
    stream.write_all(body)
}

/// Read one length-prefixed frame and decode it as JSON (the shared receive
/// half of the wire format). `Err` means the peer closed (or reset) before a
/// complete frame arrived.
fn read_frame(stream: &mut UnixStream) -> std::io::Result<Value> {
    stream.set_read_timeout(Some(Duration::from_secs(BUDGET)))?;
    let mut prefix = [0u8; 4];
    stream.read_exact(&mut prefix)?;
    let len = u32::from_be_bytes(prefix) as usize;
    assert!(
        len <= 64 * 1024,
        "test helper only reads frames within the 64 KiB cap, got {len}"
    );
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|e| std::io::Error::new(ErrorKind::InvalidData, e))
}

/// Assert the peer answered the current exchange by closing the connection
/// with no reply frame — the daemon's documented stance for malformed input.
fn assert_closed_without_reply(stream: &mut UnixStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(BUDGET)))
        .unwrap();
    let mut prefix = [0u8; 4];
    match stream.read_exact(&mut prefix) {
        Ok(()) => panic!("peer sent a reply frame; expected a silent close"),
        Err(e) => assert!(
            matches!(
                e.kind(),
                ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset
            ),
            "expected a close, got: {e}"
        ),
    }
}

/// Assert a client acquire landed on the documented hard-protocol-failure
/// classification (reachable daemon that misbehaved — never a fallback).
fn assert_protocol_failure(failure: AcquireFailure, needle: &str) {
    match &failure {
        AcquireFailure::Protocol(msg) => {
            assert!(
                msg.contains(needle),
                "protocol failure message must name {needle:?}, got: {msg}"
            );
        }
        other => panic!("expected a hard protocol failure ({needle:?}), got: {other:?}"),
    }
    assert!(
        !is_stateless_fallback(&failure),
        "a protocol failure must never route to the stateless fallback"
    );
}

// ── Fake daemon (client-half tests) ──────────────────────────────────────────

type Handler = Box<dyn FnOnce(&mut UnixStream) + Send>;

/// Bind `sock` and serve exactly `handlers.len()` connections, one handler
/// each, in order. Handlers do their own drain + reply. The returned handle
/// MUST be joined (`.expect(...)`) so an assertion panic inside a handler
/// fails the test instead of vanishing with the thread.
fn spawn_fake_daemon(sock: &Path, handlers: Vec<Handler>) -> thread::JoinHandle<()> {
    let listener = UnixListener::bind(sock).expect("bind fake daemon");
    thread::spawn(move || {
        for handler in handlers {
            let (mut stream, _) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(_) => return,
            };
            handler(&mut stream);
        }
        // Falling out of the loop drops the listener and ends the fake daemon.
    })
}

/// Drain the client's acquire frame and return it decoded. Panics (failing
/// the owning test at join time) if the client does not send exactly one
/// well-formed frame.
fn drain_acquire(stream: &mut UnixStream) -> Value {
    let mut prefix = [0u8; 4];
    stream.read_exact(&mut prefix).expect("read acquire prefix");
    let len = u32::from_be_bytes(prefix) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).expect("read acquire body");
    serde_json::from_slice(&body).expect("client sent a well-formed acquire frame")
}

fn reply_frame(stream: &mut UnixStream, body: &[u8]) {
    stream
        .write_all(&(body.len() as u32).to_be_bytes())
        .expect("write reply prefix");
    stream.write_all(body).expect("write reply body");
}

fn assignment_body(worker_id: &str, stop_fifo: &str, pid: u32, cwd: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "type": "worker_assigned",
        "worker_id": worker_id,
        "message": "warm",
        "stop_fifo": stop_fifo,
        "pid": pid,
        "cwd": cwd,
    }))
    .expect("serialize assignment")
}

fn error_body(code: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"type": "error", "error": "refused", "code": code}))
        .expect("serialize error frame")
}

/// Send `fd` to the peer as one SCM_RIGHTS control message carrying one NUL
/// byte of in-band data — the same shape `PoolServer::send_fd` uses.
fn send_fd_to(stream: &UnixStream, fd: libc::c_int) {
    let socket_fd = stream.as_raw_fd();
    let iov = [std::io::IoSlice::new(&[0u8; 1])];
    // SAFETY: mirrors src/pool.rs `PoolServer::send_fd`: one cmsghdr sized for
    // exactly one fd, one iovec pointing at live storage, sendmsg on a live
    // connected socket fd. The cmsg buffer is fully initialized before use.
    unsafe {
        let mut cmsg: libc::cmsghdr = std::mem::zeroed();
        cmsg.cmsg_len =
            (std::mem::size_of::<libc::cmsghdr>() + std::mem::size_of::<libc::c_int>()) as _;
        cmsg.cmsg_level = libc::SOL_SOCKET;
        cmsg.cmsg_type = libc::SCM_RIGHTS;

        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = iov.as_ptr() as *mut libc::iovec;
        msg.msg_iovlen = 1;
        msg.msg_control = &mut cmsg as *mut _ as *mut _;
        msg.msg_controllen = cmsg.cmsg_len;

        let data = (msg.msg_control as *mut u8).add(std::mem::size_of::<libc::cmsghdr>());
        *(data as *mut libc::c_int) = fd;

        assert!(
            libc::sendmsg(socket_fd, &msg, 0) >= 0,
            "sendmsg(SCM_RIGHTS) failed: {}",
            nix::errno::Errno::last()
        );
    }
}

// ── Wire frames: the serde layer against literal JSON ────────────────────────

#[test]
fn wire_frames_acquire_and_release_requests_parse() {
    let acquire: PoolRequest =
        serde_json::from_str(r#"{"type": "acquire", "timeout_secs": 30}"#).unwrap();
    assert!(matches!(acquire, PoolRequest::Acquire { timeout_secs: 30 }));

    // timeout_secs is optional on the wire; the documented default is 60.
    let bare: PoolRequest = serde_json::from_str(r#"{"type": "acquire"}"#).unwrap();
    assert!(matches!(
        bare,
        PoolRequest::Acquire {
            timeout_secs: DEFAULT_ACQUIRE_TIMEOUT_SECS
        }
    ));
    assert_eq!(DEFAULT_ACQUIRE_TIMEOUT_SECS, 60);

    // Unknown fields are ignored (compat rule R1) — a future client may send
    // extra request fields to a v1 daemon.
    let extra: PoolRequest =
        serde_json::from_str(r#"{"type": "acquire", "timeout_secs": 7, "future_field": {"v": 2}}"#)
            .unwrap();
    assert!(matches!(extra, PoolRequest::Acquire { timeout_secs: 7 }));

    let release: PoolRequest =
        serde_json::from_str(r#"{"type": "release", "worker_id": "w-1", "future": true}"#).unwrap();
    assert!(matches!(
        release,
        PoolRequest::Release { ref worker_id } if worker_id == "w-1"
    ));

    // The serialized spellings are exactly the documented lowercase tags.
    let acquire_json = serde_json::to_string(&PoolRequest::Acquire { timeout_secs: 1 }).unwrap();
    assert!(
        acquire_json.contains(r#""type":"acquire""#),
        "{acquire_json}"
    );
    let release_json = serde_json::to_string(&PoolRequest::Release {
        worker_id: "w".into(),
    })
    .unwrap();
    assert!(
        release_json.contains(r#""type":"release""#),
        "{release_json}"
    );
}

#[test]
fn wire_frames_malformed_request_bodies_are_parse_failures() {
    // Unknown type tag (compat rule R2): hard parse failure, not a fallback.
    let unknown = serde_json::from_str::<PoolRequest>(r#"{"type": "mystery_shape"}"#);
    assert!(unknown.is_err(), "unknown request tags must not parse");

    // Missing tag, non-objects, and null are all malformed.
    for body in [
        r#"{"timeout_secs": 5}"#,
        r#"{}"#,
        r#"[]"#,
        r#"42"#,
        r#""hello""#,
        r#"null"#,
        r#"not json at all"#,
    ] {
        assert!(
            serde_json::from_str::<PoolRequest>(body).is_err(),
            "body must not parse: {body}"
        );
    }
}

#[test]
fn wire_frames_assignment_round_trips_and_tolerates_unknown_fields() {
    let literal = json!({
        "type": "worker_assigned",
        "worker_id": "uuid-1",
        "message": "Worker ready",
        "stop_fifo": "/tmp/claude-print-1-x/stop.fifo",
        "pid": 12345,
        "cwd": "/srv/daemon",
        "future_field": {"v": 3},
    });
    let parsed: PoolResponse = serde_json::from_value(literal).unwrap();
    match parsed {
        PoolResponse::WorkerAssigned {
            worker_id,
            message,
            stop_fifo,
            pid,
            cwd,
        } => {
            assert_eq!(worker_id, "uuid-1");
            assert_eq!(message, "Worker ready");
            assert_eq!(stop_fifo, "/tmp/claude-print-1-x/stop.fifo");
            assert_eq!(pid, 12345);
            assert_eq!(cwd, "/srv/daemon");
        }
        other => panic!("wrong variant: {other:?}"),
    }

    // The serialized frame uses the documented lowercase tag and field set.
    let json = serde_json::to_string(&PoolResponse::WorkerAssigned {
        worker_id: "uuid-1".into(),
        message: "Worker ready".into(),
        stop_fifo: "/tmp/stop.fifo".into(),
        pid: 7,
        cwd: "/srv".into(),
    })
    .unwrap();
    assert!(json.contains(r#""type":"worker_assigned""#), "{json}");
    for field in ["worker_id", "message", "stop_fifo", "pid", "cwd"] {
        assert!(
            json.contains(&format!(r#""{field}""#)),
            "{field} missing: {json}"
        );
    }
}

#[test]
fn wire_frames_legacy_assignment_parses_with_empty_defaults() {
    // A daemon predating the Stop-FIFO handoff sends only the original three
    // fields. It must PARSE (that tolerance is what produces the precise
    // validation diagnostic) with the serde defaults empty.
    let legacy: PoolResponse =
        serde_json::from_str(r#"{"type": "worker_assigned", "worker_id": "w", "message": "warm"}"#)
            .unwrap();
    match legacy {
        PoolResponse::WorkerAssigned {
            worker_id,
            stop_fifo,
            pid,
            cwd,
            ..
        } => {
            assert_eq!(worker_id, "w");
            assert_eq!(stop_fifo, "");
            assert_eq!(pid, 0);
            assert_eq!(cwd, "");
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn wire_frames_error_codes_are_exact_snake_case() {
    const CASES: &[(&str, ErrorCode)] = &[
        ("pool_full", ErrorCode::PoolFull),
        ("acquire_timeout", ErrorCode::AcquireTimeout),
        ("invalid_worker_id", ErrorCode::InvalidWorkerId),
        ("internal_error", ErrorCode::InternalError),
        ("shutting_down", ErrorCode::ShuttingDown),
    ];
    for (spelling, code) in CASES {
        let typed = PoolResponse::Error {
            error: "refused".into(),
            code: *code,
        };
        let json = serde_json::to_string(&typed).unwrap();
        assert!(
            json.contains(&format!(r#""code":"{spelling}""#)),
            "{spelling}: serialized as {json}"
        );

        let literal = json!({"type": "error", "error": "refused", "code": spelling});
        let parsed: PoolResponse = serde_json::from_value(literal).unwrap();
        match parsed {
            PoolResponse::Error { code: got, .. } => assert_eq!(got, *code),
            other => panic!("wrong variant: {other:?}"),
        }
    }
}

#[test]
fn unknown_error_code_is_a_parse_failure() {
    // Compat rule R4: the code registry is closed. A newer daemon answering
    // with a code this client does not know produces a parse failure, which
    // classifies as a hard protocol failure — new codes are a breaking
    // change, and this pin is the tripwire that documents it.
    let unknown = serde_json::from_value::<PoolResponse>(json!({
        "type": "error", "error": "backing off", "code": "warmup_backoff",
    }));
    assert!(unknown.is_err(), "unknown error codes must not parse");
}

#[test]
fn unknown_response_tag_spelling_is_rejected() {
    // Tags are exact lowercase snake_case (compat rule R2); any other
    // spelling is an unknown tag and must not parse.
    for body in [
        r#"{"type": "WorkerAssigned", "worker_id": "w", "message": "m"}"#,
        r#"{"type": "WORKER_ASSIGNED", "worker_id": "w", "message": "m"}"#,
        r#"{"type": "worker-assigned", "worker_id": "w", "message": "m"}"#,
        r#"{"type": "Error", "error": "e", "code": "pool_full"}"#,
    ] {
        assert!(
            serde_json::from_str::<PoolResponse>(body).is_err(),
            "tag spelling must be rejected: {body}"
        );
    }
}

// ── Client half: real PoolClient against fake daemons ────────────────────────

#[test]
fn happy_path_yields_a_usable_fd_and_the_full_payload() {
    let dir = temp_dir("happy");
    let sock = dir.path().join("pool.sock");

    let (mine, theirs) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let mut probe = mine; // the end the test keeps; the client gets `theirs`

    let worker_id = "happy-worker";
    let handlers: Vec<Handler> = vec![
        Box::new(move |stream: &mut UnixStream| {
            let acquire = drain_acquire(stream);
            assert_eq!(acquire["type"], "acquire", "{acquire}");
            reply_frame(
                stream,
                &assignment_body(worker_id, "/tmp/happy/stop.fifo", 4242, "/srv"),
            );
            let sent = theirs.into_raw_fd();
            send_fd_to(stream, sent);
            // SAFETY: the kernel duplicated `sent` into the receiver's table
            // when sendmsg completed; close our copy so nothing leaks.
            unsafe { libc::close(sent) };
        }),
        Box::new(move |stream: &mut UnixStream| {
            // The release exchange: a NEW connection carrying the release
            // frame for the same worker id, answered with the documented
            // empty-payload worker_assigned shape.
            let mut prefix = [0u8; 4];
            stream.read_exact(&mut prefix).expect("read release prefix");
            let len = u32::from_be_bytes(prefix) as usize;
            let mut body = vec![0u8; len];
            stream.read_exact(&mut body).expect("read release body");
            let release: Value = serde_json::from_slice(&body).expect("release frame parses");
            assert_eq!(release["type"], "release", "{release}");
            assert_eq!(release["worker_id"], worker_id, "{release}");
            reply_frame(
                stream,
                serde_json::to_vec(&json!({
                    "type": "worker_assigned",
                    "worker_id": worker_id,
                    "message": "Worker released",
                    "stop_fifo": "",
                    "pid": 0,
                    "cwd": "",
                }))
                .unwrap()
                .as_slice(),
            );
        }),
    ];
    let daemon = spawn_fake_daemon(&sock, handlers);

    let mut worker = PoolClient::new(sock.clone())
        .acquire(BUDGET)
        .expect("acquire must succeed");

    // The full assignment payload survives the round trip.
    assert_eq!(worker.worker_id(), worker_id);
    assert_eq!(worker.pid().as_raw(), 4242);
    assert_eq!(worker.stop_fifo(), Path::new("/tmp/happy/stop.fifo"));
    assert_eq!(worker.worker_cwd(), Path::new("/srv"));
    assert!(!worker.is_released());

    // The transferred fd is a live, usable PTY-master stand-in: bytes written
    // on the kept end are readable through it.
    probe.write_all(b"P").expect("write probe byte");
    let mut buf = [0u8; 1];
    let mut read_bytes = 0;
    let deadline = Instant::now() + Duration::from_secs(BUDGET);
    while read_bytes < 1 {
        assert!(
            Instant::now() < deadline,
            "transferred fd never became readable"
        );
        // SAFETY: poll-then-read on the fd the client owns through
        // AcquiredWorker; this test never takes ownership of the raw fd.
        let n = unsafe { libc::read(worker.master_fd(), buf.as_mut_ptr() as *mut libc::c_void, 1) };
        if n == 1 {
            read_bytes = 1;
        } else {
            assert_eq!(n, -1, "unexpected short read");
            assert_eq!(nix::errno::Errno::last(), nix::errno::Errno::EINTR);
        }
    }
    assert_eq!(buf[0], b'P');

    // MSG_CMSG_CLOEXEC: the received fd can never leak into an exec.
    // SAFETY: F_GETFD probe on a live fd the client owns.
    let flags = unsafe { libc::fcntl(worker.master_fd(), libc::F_GETFD) };
    assert!(flags >= 0, "F_GETFD failed on the transferred fd");
    assert_ne!(
        flags & libc::FD_CLOEXEC,
        0,
        "transferred fd must be CLOEXEC"
    );

    // Explicit release, then the documented idempotence.
    worker.release();
    assert!(worker.is_released());
    worker.release(); // no second exchange, no panic

    daemon.join().expect("fake daemon thread panicked");
}

#[test]
fn assignment_missing_required_fields_is_a_hard_protocol_failure() {
    // Compat rule R3: a legacy (pre-fifo) assignment parses but is rejected
    // as a hard protocol failure naming the missing field. The fd transfer
    // SUCCEEDS in every case, so the failure is attributable to validation
    // and not to a broken transfer.
    struct Case {
        name: &'static str,
        worker_id: &'static str,
        stop_fifo: &'static str,
        pid: u32,
        cwd: &'static str,
        needle: &'static str,
    }
    let cases = [
        Case {
            name: "legacy frame without extras",
            worker_id: "w",
            stop_fifo: "",
            pid: 0,
            cwd: "",
            needle: "stop_fifo",
        },
        Case {
            name: "empty stop_fifo",
            worker_id: "w",
            stop_fifo: "",
            pid: 9,
            cwd: "/srv",
            needle: "stop_fifo",
        },
        Case {
            name: "zero pid",
            worker_id: "w",
            stop_fifo: "/tmp/s.fifo",
            pid: 0,
            cwd: "/srv",
            needle: "pid",
        },
        Case {
            name: "empty cwd",
            worker_id: "w",
            stop_fifo: "/tmp/s.fifo",
            pid: 9,
            cwd: "",
            needle: "cwd",
        },
        Case {
            name: "empty worker_id",
            worker_id: "",
            stop_fifo: "/tmp/s.fifo",
            pid: 9,
            cwd: "/srv",
            needle: "worker_id",
        },
    ];

    for case in cases {
        let dir = temp_dir("missing-field");
        let sock = dir.path().join("pool.sock");
        let body = assignment_body(case.worker_id, case.stop_fifo, case.pid, case.cwd);
        let daemon = spawn_fake_daemon(
            &sock,
            vec![Box::new(move |stream: &mut UnixStream| {
                drain_acquire(stream);
                reply_frame(stream, &body);
                let (_, theirs) = std::os::unix::net::UnixStream::pair().expect("socketpair");
                let sent = theirs.into_raw_fd();
                send_fd_to(stream, sent);
                // SAFETY: the kernel holds the receiver's copy after the
                // SCM_RIGHTS send; close ours so nothing leaks.
                unsafe { libc::close(sent) };
            })],
        );

        let failure = PoolClient::new(sock.clone())
            .acquire(BUDGET)
            .expect_err(&format!("{}: must be a protocol failure", case.name));
        assert_protocol_failure(failure, case.needle);

        daemon.join().expect("fake daemon thread panicked");
    }
}

#[test]
fn unknown_response_shape_is_a_hard_protocol_failure() {
    let dir = temp_dir("wrong-shape");
    let sock = dir.path().join("pool.sock");
    let body: &[u8] = br#"{"type":"mystery_shape"}"#;
    let daemon = spawn_fake_daemon(
        &sock,
        vec![Box::new(move |stream: &mut UnixStream| {
            drain_acquire(stream);
            reply_frame(stream, body);
        })],
    );
    let failure = PoolClient::new(sock.clone())
        .acquire(BUDGET)
        .expect_err("an unknown response tag is a protocol failure");
    assert_protocol_failure(failure, "malformed response");
    daemon.join().expect("fake daemon thread panicked");
}

#[test]
fn garbage_body_is_a_hard_protocol_failure() {
    let dir = temp_dir("garbage");
    let sock = dir.path().join("pool.sock");
    let body: &[u8] = b"\x00\xff definitely not json";
    let daemon = spawn_fake_daemon(
        &sock,
        vec![Box::new(move |stream: &mut UnixStream| {
            drain_acquire(stream);
            reply_frame(stream, body);
        })],
    );
    let failure = PoolClient::new(sock.clone())
        .acquire(BUDGET)
        .expect_err("a non-JSON body is a protocol failure");
    assert_protocol_failure(failure, "malformed response");
    daemon.join().expect("fake daemon thread panicked");
}

#[test]
fn oversized_response_prefix_is_rejected() {
    // The client applies the same 64 KiB cap the daemon applies to requests:
    // a hostile prefix must be rejected before the body is allocated.
    let dir = temp_dir("oversized");
    let sock = dir.path().join("pool.sock");
    let daemon = spawn_fake_daemon(
        &sock,
        vec![Box::new(move |stream: &mut UnixStream| {
            drain_acquire(stream);
            stream
                .write_all(&((64 * 1024 + 1) as u32).to_be_bytes())
                .expect("write oversized prefix");
        })],
    );
    let failure = PoolClient::new(sock.clone())
        .acquire(BUDGET)
        .expect_err("an oversized response prefix is a protocol failure");
    assert_protocol_failure(failure, "exceeds");
    daemon.join().expect("fake daemon thread panicked");
}

#[test]
fn every_documented_error_code_maps_to_its_wire_spelling_and_falls_back() {
    const SPELLINGS: &[(&str, &str)] = &[
        ("pool_full", "pool_full"),
        ("acquire_timeout", "acquire_timeout"),
        ("invalid_worker_id", "invalid_worker_id"),
        ("internal_error", "internal_error"),
        ("shutting_down", "shutting_down"),
    ];
    for (spelling, expected) in SPELLINGS {
        let dir = temp_dir("refusal");
        let sock = dir.path().join("pool.sock");
        let body = error_body(spelling);
        let daemon = spawn_fake_daemon(
            &sock,
            vec![Box::new(move |stream: &mut UnixStream| {
                drain_acquire(stream);
                reply_frame(stream, &body);
            })],
        );
        let failure = PoolClient::new(sock.clone())
            .acquire(BUDGET)
            .expect_err(&format!(
                "a well-formed {spelling} refusal must not be an Ok"
            ));
        match &failure {
            AcquireFailure::PoolUnavailable { code, error } => {
                assert_eq!(code.as_str(), *expected, "code mapping");
                assert_eq!(error, "refused");
            }
            other => panic!("{spelling}: expected PoolUnavailable, got {other:?}"),
        }
        assert!(
            is_stateless_fallback(&failure),
            "a well-formed refusal routes to the stateless fallback (INV-10)"
        );
        daemon.join().expect("fake daemon thread panicked");
    }
}

#[test]
fn an_unknown_error_code_is_a_hard_protocol_failure() {
    // Compat rule R4, client half: a newer daemon's unknown refusal code is
    // a parse failure, hence a hard protocol failure — NOT a fallback.
    let dir = temp_dir("future-code");
    let sock = dir.path().join("pool.sock");
    let body = error_body("warmup_backoff");
    let daemon = spawn_fake_daemon(
        &sock,
        vec![Box::new(move |stream: &mut UnixStream| {
            drain_acquire(stream);
            reply_frame(stream, &body);
        })],
    );
    let failure = PoolClient::new(sock.clone())
        .acquire(BUDGET)
        .expect_err("an unknown code must fail");
    assert_protocol_failure(failure, "malformed response");
    daemon.join().expect("fake daemon thread panicked");
}

#[test]
fn daemon_close_mid_response_is_a_hard_protocol_failure() {
    let dir = temp_dir("close-mid");
    let sock = dir.path().join("pool.sock");
    let daemon = spawn_fake_daemon(
        &sock,
        vec![Box::new(move |stream: &mut UnixStream| {
            // Drain the request, then close without answering.
            drain_acquire(stream);
        })],
    );
    let failure = PoolClient::new(sock.clone())
        .acquire(BUDGET)
        .expect_err("a daemon that closes mid-response is a protocol failure");
    assert_protocol_failure(failure, "closed the connection");
    daemon.join().expect("fake daemon thread panicked");
}

#[test]
fn silent_daemon_fails_at_the_client_deadline() {
    // INV-12's wire half: the budget bounds the WHOLE exchange, so a daemon
    // that accepts and then goes silent cannot stall the client past it.
    let dir = temp_dir("silent");
    let sock = dir.path().join("pool.sock");
    let daemon = spawn_fake_daemon(
        &sock,
        vec![Box::new(move |stream: &mut UnixStream| {
            drain_acquire(stream);
            thread::sleep(Duration::from_secs(10));
            // Dropping the stream closes it; the client has long since
            // failed at its deadline.
        })],
    );
    let started = Instant::now();
    let failure = PoolClient::new(sock.clone())
        .acquire(SILENCE_BUDGET)
        .expect_err("silence past the deadline must fail");
    let elapsed = started.elapsed();
    assert_protocol_failure(failure, "timed out");
    assert!(
        elapsed < Duration::from_secs(8),
        "the failure must land at the client budget, took {elapsed:?}"
    );
    // Detached on purpose: the handler sleeps past this test's lifetime and
    // asserts nothing, so there is nothing to join on.
    drop(daemon);
}

#[test]
fn client_sends_the_documented_acquire_frame() {
    // The client's own half of the wire: exactly one acquire frame, tagged
    // `acquire`, carrying the budget it was given as timeout_secs.
    let dir = temp_dir("client-frame");
    let sock = dir.path().join("pool.sock");
    let daemon = spawn_fake_daemon(
        &sock,
        vec![Box::new(move |stream: &mut UnixStream| {
            let acquire = drain_acquire(stream);
            assert_eq!(acquire["type"], "acquire", "{acquire}");
            assert_eq!(
                acquire["timeout_secs"], 7,
                "the client must send its budget"
            );
            reply_frame(stream, &error_body("pool_full"));
        })],
    );
    let failure = PoolClient::new(sock.clone()).acquire(7).unwrap_err();
    assert!(matches!(
        failure,
        AcquireFailure::PoolUnavailable { ref code, .. } if code.as_str() == "pool_full"
    ));
    daemon.join().expect("fake daemon thread panicked");
}

#[test]
fn dropping_the_worker_releases_it() {
    // Drop is the release path for every exit that did not reach an explicit
    // release: the daemon must observe the release exchange on a new
    // connection.
    let dir = temp_dir("drop-release");
    let sock = dir.path().join("pool.sock");
    let worker_id = "drop-worker";
    let handlers: Vec<Handler> = vec![
        Box::new(move |stream: &mut UnixStream| {
            drain_acquire(stream);
            reply_frame(
                stream,
                &assignment_body(worker_id, "/tmp/d/stop.fifo", 77, "/srv"),
            );
            let (_, theirs) = std::os::unix::net::UnixStream::pair().expect("socketpair");
            let sent = theirs.into_raw_fd();
            send_fd_to(stream, sent);
            // SAFETY: the kernel holds the receiver's copy after the
            // SCM_RIGHTS send; close ours so nothing leaks.
            unsafe { libc::close(sent) };
        }),
        Box::new(move |stream: &mut UnixStream| {
            let reply = read_frame(stream).expect("read the drop-triggered release frame");
            assert_eq!(reply["type"], "release", "{reply}");
            assert_eq!(reply["worker_id"], worker_id, "{reply}");
            // Answer so the daemon-side connection thread completes cleanly.
            reply_frame(stream, &assignment_body(worker_id, "", 0, ""));
        }),
    ];
    let daemon = spawn_fake_daemon(&sock, handlers);

    let worker = PoolClient::new(sock.clone())
        .acquire(BUDGET)
        .expect("acquire must succeed");
    assert!(!worker.is_released());
    drop(worker); // the Drop impl must run the release exchange

    daemon.join().expect("fake daemon thread panicked");
}

#[test]
fn missing_socket_is_unreachable_and_falls_back() {
    // The classification seam: no daemon at the path is `Unreachable`, the
    // documented stateless-fallback class — never a hard error.
    let dir = temp_dir("missing");
    let sock = dir.path().join("never-bound.sock");
    let failure = PoolClient::new(sock.clone())
        .acquire(BUDGET)
        .expect_err("an absent socket must fail");
    match &failure {
        AcquireFailure::Unreachable { socket, .. } => assert_eq!(socket, &sock),
        other => panic!("expected Unreachable, got {other:?}"),
    }
    assert!(is_stateless_fallback(&failure));
}

#[test]
fn invocation_classification_seams() {
    // NotRequested: the flag absent means no pool code runs at all.
    match acquire_for_invocation(None, 30).expect("None must be Ok(NotRequested)") {
        InvocationAcquisition::NotRequested => {}
        other => panic!("expected NotRequested, got {other:?}"),
    }

    // Fallback: an unreachable pool is an Ok(Fallback) carrying a
    // stateless-fallback failure.
    let dir = temp_dir("invocation");
    let missing = dir.path().join("never-bound.sock");
    match acquire_for_invocation(Some(&missing), 30).expect("unreachable must be Ok(Fallback)") {
        InvocationAcquisition::Fallback(failure) => {
            assert!(is_stateless_fallback(&failure), "{failure:?}");
        }
        other => panic!("expected Fallback, got {other:?}"),
    }

    // Hard error: a reachable but broken daemon propagates its protocol
    // failure out of the invocation seam — never a fallback (INV-10).
    let sock = dir.path().join("broken.sock");
    let body: &[u8] = br#"{"type":"mystery_shape"}"#;
    let daemon = spawn_fake_daemon(
        &sock,
        vec![Box::new(move |stream: &mut UnixStream| {
            drain_acquire(stream);
            reply_frame(stream, body);
        })],
    );
    let err = acquire_for_invocation(Some(&sock), 30)
        .expect_err("a broken daemon must hard-error the invocation");
    assert_protocol_failure(err, "malformed response");
    daemon.join().expect("fake daemon thread panicked");
}

// ── Daemon half: real PoolServer against a raw wire client ───────────────────

/// A real `PoolServer` on a temp socket, with a manager that can never spawn
/// a worker: the claude_bin path carries an interior NUL, which fails
/// `CString::new` in `create_worker` BEFORE any fork or openpty, so the pool
/// stays deterministically empty (an acquire is answered `pool_full`) and the
/// failed attempts' hook-installer temp dirs self-clean on drop. Every test
/// shuts its daemon down, and Drop covers the panic paths, so no maintain
/// loop outlives its test.
struct TestDaemon {
    manager: Arc<Mutex<PoolManager>>,
    handle: Option<thread::JoinHandle<()>>,
    sock: PathBuf,
    _dir: tempfile::TempDir,
}

impl TestDaemon {
    fn start(tag: &str) -> Self {
        let dir = temp_dir(tag);
        let sock = dir.path().join("pool.sock");
        let manager = PoolManager::new(1, PathBuf::from("/nonexistent\0claude-print-test"), false);
        let mut server = PoolServer::new(Some(sock.to_string_lossy().into_owned()), manager, false);
        let manager = server.manager().clone();
        let handle = thread::spawn(move || {
            let _ = server.run();
            server.cleanup();
        });
        let daemon = Self {
            manager,
            handle: Some(handle),
            sock,
            _dir: dir,
        };
        let deadline = Instant::now() + TEST_CEILING;
        while !daemon.sock.exists() {
            assert!(
                Instant::now() < deadline,
                "pool server never bound its socket"
            );
            thread::sleep(Duration::from_millis(10));
        }
        daemon
    }

    fn raw_connect(&self) -> std::io::Result<UnixStream> {
        let stream = UnixStream::connect(&self.sock)?;
        stream.set_read_timeout(Some(Duration::from_secs(BUDGET)))?;
        stream.set_write_timeout(Some(Duration::from_secs(BUDGET)))?;
        Ok(stream)
    }

    fn shutdown(mut self) {
        self.manager.lock().unwrap().shutdown();
        if let Some(handle) = self.handle.take() {
            handle.join().expect("pool server thread panicked");
        }
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        // Panic-path hygiene: stop the maintain loop and reap the thread.
        self.manager.lock().unwrap().shutdown();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[test]
fn daemon_side_empty_pool_refuses_acquire_with_pool_full() {
    let daemon = TestDaemon::start("pool-full");
    let mut stream = daemon.raw_connect().expect("connect to the pool server");
    send_frame(&mut stream, br#"{"type":"acquire","timeout_secs":5}"#)
        .expect("write the acquire frame");
    let reply = read_frame(&mut stream).expect("read the refusal frame");
    assert_eq!(reply["type"], "error", "{reply}");
    assert_eq!(reply["code"], "pool_full", "{reply}");
    assert!(
        reply["error"].as_str().unwrap().contains("Pool full"),
        "{reply}"
    );
    daemon.shutdown();
}

#[test]
fn daemon_side_release_unknown_worker_id_refuses_invalid_worker_id() {
    let daemon = TestDaemon::start("invalid-id");
    let mut stream = daemon.raw_connect().expect("connect to the pool server");
    send_frame(
        &mut stream,
        br#"{"type":"release","worker_id":"no-such-worker"}"#,
    )
    .expect("write the release frame");
    let reply = read_frame(&mut stream).expect("read the refusal frame");
    assert_eq!(reply["type"], "error", "{reply}");
    assert_eq!(reply["code"], "invalid_worker_id", "{reply}");
    daemon.shutdown();
}

#[test]
fn daemon_side_oversized_request_is_closed_without_a_reply() {
    let daemon = TestDaemon::start("oversized-req");
    let mut stream = daemon.raw_connect().expect("connect to the pool server");
    // A prefix past the 64 KiB cap is rejected before the body is read.
    stream
        .write_all(&100_000u32.to_be_bytes())
        .expect("write oversized prefix");
    assert_closed_without_reply(&mut stream);
    daemon.shutdown();
}

#[test]
fn daemon_side_malformed_request_is_closed_without_a_reply_and_the_daemon_stays_healthy() {
    let daemon = TestDaemon::start("malformed-req");
    let mut stream = daemon.raw_connect().expect("connect to the pool server");
    send_frame(&mut stream, b"this is not json").expect("write the frame");
    assert_closed_without_reply(&mut stream);

    // The daemon itself is unaffected: the next connection is served.
    let mut stream = daemon.raw_connect().expect("reconnect to the pool server");
    send_frame(&mut stream, br#"{"type":"acquire","timeout_secs":5}"#)
        .expect("write the acquire frame");
    let reply =
        read_frame(&mut stream).expect("daemon must stay healthy after a malformed request");
    assert_eq!(reply["code"], "pool_full", "{reply}");
    daemon.shutdown();
}

#[test]
fn daemon_side_unknown_request_type_is_closed_without_a_reply() {
    let daemon = TestDaemon::start("unknown-req");
    let mut stream = daemon.raw_connect().expect("connect to the pool server");
    send_frame(&mut stream, br#"{"type":"teleport"}"#).expect("write the frame");
    assert_closed_without_reply(&mut stream);

    let mut stream = daemon.raw_connect().expect("reconnect to the pool server");
    send_frame(&mut stream, br#"{"type":"acquire","timeout_secs":5}"#)
        .expect("write the acquire frame");
    let reply = read_frame(&mut stream).expect("daemon must stay healthy after an unknown tag");
    assert_eq!(reply["code"], "pool_full", "{reply}");
    daemon.shutdown();
}

#[test]
fn daemon_side_empty_connection_is_a_clean_close() {
    // Connecting and closing without sending anything is a no-op for the
    // daemon — the Ok(None) EOF path, not an error, and definitely not a
    // connection-thread spin.
    let daemon = TestDaemon::start("empty-conn");
    drop(daemon.raw_connect());
    thread::sleep(Duration::from_millis(100));

    let mut stream = daemon.raw_connect().expect("connect to the pool server");
    send_frame(&mut stream, br#"{"type":"acquire","timeout_secs":5}"#)
        .expect("write the acquire frame");
    let reply =
        read_frame(&mut stream).expect("daemon must stay healthy after an empty connection");
    assert_eq!(reply["code"], "pool_full", "{reply}");
    daemon.shutdown();
}

#[test]
fn daemon_side_shutdown_refuses_and_the_daemon_cleans_the_socket() {
    let daemon = TestDaemon::start("shutdown");
    daemon.manager.lock().unwrap().shutdown();

    // Three outcomes are all the shutdown contract, depending on where the
    // accept loop was when the flag flipped:
    //   * the loop already stopped → connect refused,
    //   * the loop stopped between accept and reply, or the listener closed
    //     with the connection pending → the exchange errors out (reset/EOF),
    //   * the connection is served normally → the documented `shutting_down`
    //     refusal frame.
    match daemon.raw_connect() {
        Err(_) => {}
        Ok(mut stream) => {
            if send_frame(&mut stream, br#"{"type":"acquire","timeout_secs":5}"#).is_ok() {
                // A read error here means the listener closed with this
                // connection unanswered — also part of the contract.
                if let Ok(reply) = read_frame(&mut stream) {
                    assert_eq!(reply["type"], "error", "{reply}");
                    assert_eq!(reply["code"], "shutting_down", "{reply}");
                }
            }
        }
    }

    // Teardown removes the socket file this daemon created (ownership-checked
    // cleanup), within the accept tick.
    let deadline = Instant::now() + TEST_CEILING;
    while daemon.sock.exists() {
        assert!(
            Instant::now() < deadline,
            "the daemon never removed its own socket file"
        );
        thread::sleep(Duration::from_millis(10));
    }
    daemon.shutdown();
}
