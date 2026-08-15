use claude_print::terminal::TerminalEmu;

fn emu() -> TerminalEmu {
    TerminalEmu::new(50, 220)
}

#[test]
fn da1_responds_with_csi_6c() {
    let mut e = emu();
    assert_eq!(e.feed(b"\x1b[c"), b"\x1b[?6c");
}

#[test]
fn da2_responds_with_secondary_attrs() {
    let mut e = emu();
    assert_eq!(e.feed(b"\x1b[>c"), b"\x1b[>0;0;0c");
}

#[test]
fn dsr_responds_with_cursor_pos() {
    let mut e = emu();
    assert_eq!(e.feed(b"\x1b[6n"), b"\x1b[1;1R");
}

#[test]
fn xtversion_responds_with_dcs_string() {
    let mut e = emu();
    assert_eq!(e.feed(b"\x1b[>q"), b"\x1bP>|claude-print\x1b\\");
}

#[test]
fn window_size_responds_with_configured_dimensions() {
    let mut e = emu();
    // rows=50, cols=220 → ESC[8;50;220t
    assert_eq!(e.feed(b"\x1b[18t"), b"\x1b[8;50;220t");
}

#[test]
fn multiple_probes_in_one_chunk_answered_in_order() {
    let mut e = emu();
    let resp = e.feed(b"\x1b[c\x1b[6n\x1b[>c");
    assert_eq!(resp, b"\x1b[?6c\x1b[1;1R\x1b[>0;0;0c");
}

#[test]
fn probe_dedup_da1_answered_only_once() {
    let mut e = emu();
    let first = e.feed(b"\x1b[c");
    let second = e.feed(b"\x1b[c");
    assert_eq!(first, b"\x1b[?6c", "first DA1 should be answered");
    assert_eq!(second, b"", "second DA1 should be suppressed by dedup");
}

#[test]
fn unknown_probe_ignored_no_response_no_panic() {
    let mut e = emu();
    let resp = e.feed(b"\x1b[99t");
    assert_eq!(
        resp, b"",
        "unknown escape sequence must produce no response"
    );
}

#[test]
fn split_chunk_probe_answered_on_second_read() {
    let mut e = emu();
    let first = e.feed(b"\x1b[");
    let second = e.feed(b"c");
    assert_eq!(first, b"", "partial probe should produce no response yet");
    assert_eq!(
        second, b"\x1b[?6c",
        "probe completed on second read should be answered"
    );
}

// bf-l69i: Test that overly long sequences don't panic on unwrap()
#[test]
fn overly_long_sequence_no_panic() {
    let mut e = emu();
    // Feed a sequence that exceeds MAX_PROBE_LEN (32)
    let long_seq = b"\x1b[".repeat(20);
    let resp = e.feed(&long_seq);
    // Should not panic, should return empty response
    assert_eq!(resp, b"");
}

// bf-l69i: Test that sequences with invalid bytes don't panic
#[test]
fn invalid_bytes_no_panic() {
    let mut e = emu();
    // Feed a sequence with invalid intermediate bytes
    let resp = e.feed(b"\x1b[\xff\xff\xff\xff\xff\xff");
    // Should not panic on unwrap at line 86
    assert_eq!(resp, b"");
}

// bf-l69i: Test empty buffer state is handled correctly
#[test]
fn empty_buffer_handled() {
    let mut e = emu();
    // Start with a partial sequence
    let resp1 = e.feed(b"\x1b");
    assert_eq!(resp1, b"");
    // Feed an invalid byte that should clear and potentially check for ESC
    let resp2 = e.feed(b"\xff");
    // Should not panic on unwrap at line 86
    assert_eq!(resp2, b"");
}

// Test that feeding empty chunks doesn't panic
#[test]
fn empty_chunk_no_panic() {
    let mut e = emu();
    // Feed empty chunk - should not panic on buf.first() in check_state
    let resp = e.feed(b"");
    assert_eq!(resp, b"");
}

// Test that single byte chunks are handled correctly
#[test]
fn single_byte_chunk_no_panic() {
    let mut e = emu();
    // Feed just ESC - should return incomplete, not panic
    let resp = e.feed(b"\x1b");
    assert_eq!(resp, b"");

    // Feed a random non-ESC byte - should not accumulate or panic
    let resp2 = e.feed(b"X");
    assert_eq!(resp2, b"");
}

// Test two-byte sequences that start CSI but are incomplete
#[test]
fn two_byte_csi_incomplete_no_panic() {
    let mut e = emu();
    // Feed just CSI start (ESC + [) - should return incomplete
    let resp = e.feed(b"\x1b[");
    assert_eq!(resp, b"");

    // Verify it's waiting for more data
    let resp2 = e.feed(b"c");
    assert_eq!(resp2, b"\x1b[?6c");
}

// Test that malformed CSI sequences don't panic
#[test]
fn malformed_csi_no_panic() {
    let mut e = emu();
    // Invalid byte in CSI parameter position
    let resp = e.feed(b"\x1b[\x00\x00c");
    // Should not panic, just return empty
    assert_eq!(resp, b"");
}
