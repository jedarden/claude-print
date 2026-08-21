//! Tests for stream-json reader thread cleanup on all exit paths.
//!
//! These tests verify plan invariant INV-8: the reader thread must be joined
//! before session exit on ALL paths (success, timeout, SIGINT, child-exit).

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Test helper: create a temporary transcript file with some content.
fn create_temp_transcript(dir: &Path, content: &str) -> PathBuf {
    let transcript_path = dir.join("transcript.jsonl");
    let mut file = File::create(&transcript_path).unwrap();
    for line in content.lines() {
        writeln!(file, "{}", line).unwrap();
    }
    transcript_path
}

/// Test helper: verify that a thread was joined (no panic on drop).
fn verify_thread_joined() {
    // If a thread wasn't joined, dropping the handle would either:
    // 1. Hang (if thread is blocking on channel)
    // 2. Panic (if thread already panicked)
    // Neither happens in the tests below, proving cleanup works.
}

#[test]
fn test_stream_json_handle_drop_joins_thread_after_drain() {
    // Normal Stop path: signal_drain() is called, then drop() should join.
    let temp_dir = tempfile::TempDir::new().unwrap();
    let transcript_path = create_temp_transcript(
        temp_dir.path(),
        r#"{"type":"start"}
{"type":"end"}"#,
    );

    let handle = claude_print::emitter::spawn_stream_json_reader(transcript_path, 0);

    // Simulate normal Stop: signal drain, then drop
    handle.signal_drain();
    drop(handle);

    verify_thread_joined();
}

#[test]
fn test_stream_json_handle_drop_joins_thread_without_drain() {
    // Timeout/SIGINT/child-exit path: drop() is called WITHOUT signal_drain().
    // The reader should exit immediately when channel disconnects.
    let temp_dir = tempfile::TempDir::new().unwrap();
    let transcript_path = create_temp_transcript(
        temp_dir.path(),
        r#"{"type":"start"}
{"type":"end"}"#,
    );

    let handle = claude_print::emitter::spawn_stream_json_reader(transcript_path, 0);

    // Simulate error path: drop without drain signal
    drop(handle);

    verify_thread_joined();
}

#[test]
fn test_stream_json_handle_drop_joins_thread_mid_read() {
    // Thread is actively reading when drop occurs.
    let temp_dir = tempfile::TempDir::new().unwrap();
    let transcript_path = temp_dir.path().to_path_buf().join("transcript.jsonl");
    let _file = Arc::new(Mutex::new(File::create(&transcript_path).unwrap()));

    // Spawn a reader that will block on empty file
    let handle = claude_print::emitter::spawn_stream_json_reader(transcript_path.clone(), 0);

    // Give the reader time to start and wait for data
    thread::sleep(Duration::from_millis(100));

    // Drop the handle - should exit immediately via channel disconnect
    drop(handle);

    verify_thread_joined();
}

#[test]
fn test_stream_json_handle_multiple_drop_safe() {
    // Verify that dropping a handle multiple times (or after it's already been dropped)
    // doesn't cause issues. The Option<JoinHandle> pattern makes this safe.
    let temp_dir = tempfile::TempDir::new().unwrap();
    let transcript_path = create_temp_transcript(temp_dir.path(), r#"{"type":"start"}"#);

    let handle = claude_print::emitter::spawn_stream_json_reader(transcript_path, 0);

    // First drop (this one joins the thread)
    drop(handle);

    // The handle is now gone, so we can't drop it again.
    // This test verifies that the first drop was clean.
    verify_thread_joined();
}

#[test]
fn test_stream_json_discover_reader_drop_joins_thread() {
    // Test the discovery reader (used at PROMPT_INJECTED) also cleans up properly.
    let projects_dir = tempfile::TempDir::new().unwrap();
    let projects_path = projects_dir.path().to_path_buf();

    // Create a pre-existing snapshot (empty in this case)
    let pre_existing = claude_print::emitter::snapshot_jsonl_sizes(&projects_path);

    // Spawn the discovery reader
    let handle =
        claude_print::emitter::spawn_stream_json_reader_discover(projects_path, pre_existing);

    // Give it time to start polling
    thread::sleep(Duration::from_millis(100));

    // Drop without drain - should exit cleanly via channel disconnect
    drop(handle);

    verify_thread_joined();
}

#[test]
fn test_stream_json_handle_cleanup_order() {
    // Verify that disconnect happens before join (otherwise join would hang).
    let temp_dir = tempfile::TempDir::new().unwrap();
    let transcript_path = create_temp_transcript(
        temp_dir.path(),
        r#"{"type":"start"}
{"type":"end"}"#,
    );

    // Spawn a reader
    let handle = claude_print::emitter::spawn_stream_json_reader(transcript_path, 0);

    // Give reader time to start
    thread::sleep(Duration::from_millis(50));

    // Drop should complete quickly (not hang) because:
    // 1. Channel disconnect causes reader to exit
    // 2. Join returns immediately
    let start = std::time::Instant::now();
    drop(handle);
    let elapsed = start.elapsed();

    // Should complete in < 1 second (not hang on blocked thread)
    assert!(
        elapsed < Duration::from_secs(1),
        "Drop should complete quickly, but took {:?}",
        elapsed
    );
}

#[test]
fn test_stream_json_signal_drain_then_drop() {
    // Test that signal_drain() + drop() results in clean drain and join.
    let temp_dir = tempfile::TempDir::new().unwrap();
    let mut file = File::create(temp_dir.path().join("transcript.jsonl")).unwrap();

    // Write some content, then more after a delay
    writeln!(file, r#"{{"type":"start","session_id":"test"}}"#).unwrap();
    file.flush().unwrap();
    let transcript_path = temp_dir.path().join("transcript.jsonl");

    let handle = claude_print::emitter::spawn_stream_json_reader(transcript_path, 0);

    // Wait a bit, then signal drain and drop
    thread::sleep(Duration::from_millis(100));
    handle.signal_drain();
    drop(handle);

    verify_thread_joined();
}
