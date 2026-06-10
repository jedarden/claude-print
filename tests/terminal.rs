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
