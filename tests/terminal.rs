//! Probe-responder contract tests (bead claudepr-883e46a9).
//!
//! Pins docs/notes/terminal-probes.md against src/terminal.rs: the doc's probe
//! table (exact response bytes, split-chunk reassembly, dedup bitmask,
//! MAX_PROBE_LEN discard, unknown-sequence silence) and the startup
//! sequencer's idle fallback (src/startup.rs: no dialog on screen, ≥ 200
//! bytes received, quiet window elapsed → bare CR).
//!
//! The TUI side is version-pinned: `tests/fixtures/terminal_probes_v2.1.282.json`
//! holds a startup capture of claude 2.1.282 (2026-09-24) taken with
//! `scripts/probe-tui-terminal-probes.py --answer` — the driver replies via a
//! faithful port of this responder, so the capture is the production probe
//! shape. On 2.1.282 the TUI sends XTVERSION once, DA1 twice (the retry must
//! be dedup-suppressed), and several sequences the doc table does not list
//! (`?u`, `?2026$p`, `16t`, `?1016$p`, SGR, mode sets) that must stay silent.
//! The previous 2.1.270 capture (2026-09-14) is retained as the compatibility
//! baseline and its probe inventory is asserted identical to the pinned one
//! (see `retained_prior_capture_probe_inventory_matches_the_pinned_shape`).
//! Re-run the script after any Claude Code update and re-pin fixture + tests +
//! doc together, exactly like `tests/fixtures/claude_contracts_v2.1.282.json`
//! (docs/notes/claude-contract-probes.md §Maintenance).

use std::collections::HashSet;
use std::time::Duration;

use claude_print::startup::{StartupAction, StartupSeq};
use claude_print::terminal::TerminalEmu;
use serde_json::Value;

/// claude-print's stty fallback dimensions (docs/notes/terminal-probes.md).
const ROWS: u16 = 50;
const COLS: u16 = 220;

/// Version-pinned capture of the real claude TUI startup probe traffic. The
/// `v<version>` filename stamp and [`FIXTURE_VERSION`] must move together.
const FIXTURE: &str = include_str!("fixtures/terminal_probes_v2.1.282.json");
const FIXTURE_VERSION: &str = "2.1.282";

/// The prior 2.1.270 capture, retained as the compatibility baseline: its
/// probe inventory is asserted identical to the pinned one by
/// [`retained_prior_capture_probe_inventory_matches_the_pinned_shape`], so a
/// future re-pin that changes the recognized probe shape cannot silently drop
/// the only evidence the two versions agreed.
const PRIOR_FIXTURE: &str = include_str!("fixtures/terminal_probes_v2.1.270.json");
const PRIOR_FIXTURE_VERSION: &str = "2.1.270";

/// One row of the doc's probe table: (name, probe bytes, alternate spelling
/// answering the same dedup bit, documented response bytes).
type ProbeRow = (
    &'static str,
    &'static [u8],
    Option<&'static [u8]>,
    &'static [u8],
);

/// The probe table from docs/notes/terminal-probes.md.
const PROBE_TABLE: &[ProbeRow] = &[
    ("DA1", b"\x1b[c", Some(b"\x1b[0c"), b"\x1b[?6c"),
    ("DA2", b"\x1b[>c", Some(b"\x1b[>0c"), b"\x1b[>0;0;0c"),
    ("DSR", b"\x1b[6n", None, b"\x1b[1;1R"),
    (
        "XTVERSION",
        b"\x1b[>q",
        Some(b"\x1b[>0q"),
        b"\x1bP>|claude-print\x1b\\",
    ),
    ("WinSize", b"\x1b[18t", None, b"\x1b[8;50;220t"),
];

/// Sequences the responder must stay silent on — includes ones the real TUI
/// actually sent in the pinned captures (`?u`, `?2026$p` in both 2.1.270 and
/// 2.1.282; `16t` and `?1016$p` new in the 2.1.282 capture). `16t` is the
/// XTWINOPS cell-size-in-pixels query — the sibling of the answered `18t`
/// chars-size probe, deliberately unanswered.
const UNKNOWN_SEQUENCES: &[&[u8]] = &[
    b"\x1b[99t",            // unknown mode
    b"\x1b[?25h",           // cursor show (capture)
    b"\x1b[?1049h",         // alternate screen (capture)
    b"\x1b[?2004h",         // bracketed paste on (capture)
    b"\x1b[2J",             // erase screen (capture)
    b"\x1b[38;5;174m",      // SGR color (capture)
    b"\x1b[?u",             // kitty keyboard query (capture)
    b"\x1b[?2026$p",        // synchronized-output query (capture)
    b"\x1b[16t",            // cell-size-in-pixels query — 2.1.282 capture
    b"\x1b[?1016$p",        // synchronized-output pixel query — 2.1.282 capture
    b"\x1bP1;2|body\x1b\\", // DCS string — not '['-introduced, never answered
];

fn emu() -> TerminalEmu {
    TerminalEmu::new(ROWS, COLS)
}

fn response_for(name: &str) -> &'static [u8] {
    match PROBE_TABLE.iter().find(|row| row.0 == name) {
        Some(&(_, _, _, resp)) => resp,
        None => panic!("probe kind {name} must be a documented table row"),
    }
}

/// Inverse of the capture script's `esc()`: `\xHH` escapes → raw bytes.
fn unesc(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1] == b'x' {
            out.push(
                u8::from_str_radix(&s[i + 2..i + 4], 16)
                    .expect("fixture escapes must be two hex digits"),
            );
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

// ── Doc probe table: exact responses ────────────────────────────────────────

#[test]
fn doc_table_every_probe_yields_documented_response_bytes() {
    for &(name, probe, _, resp) in PROBE_TABLE {
        let mut e = emu();
        assert_eq!(
            e.feed(probe),
            resp,
            "{name}: response must match docs/notes/terminal-probes.md exactly"
        );
    }
}

#[test]
fn doc_table_parameterized_spelling_yields_the_same_response() {
    for &(name, _, alt, resp) in PROBE_TABLE {
        let Some(alt) = alt else { continue };
        let mut e = emu();
        assert_eq!(
            e.feed(alt),
            resp,
            "{name}: the lenient (parameterized) spelling must answer with the same bytes"
        );
    }
}

// ── Split-chunk reassembly ──────────────────────────────────────────────────

#[test]
fn probe_split_at_every_byte_boundary_is_reassembled_and_answered_once() {
    for &(name, probe, alt, resp) in PROBE_TABLE {
        for spelling in [probe].into_iter().chain(alt) {
            for split in 1..spelling.len() {
                let mut e = emu();
                let head = e.feed(&spelling[..split]);
                assert!(
                    head.is_empty(),
                    "{name} split at byte {split}: no response may precede the final byte"
                );
                let tail = e.feed(&spelling[split..]);
                assert_eq!(
                    tail, resp,
                    "{name} split at byte {split}: reassembled probe must be answered exactly once, with the documented bytes"
                );
            }
        }
    }
}

// ── Dedup bitmask ───────────────────────────────────────────────────────────

#[test]
fn repeated_probe_is_suppressed_by_dedup_bitmask() {
    for &(name, probe, alt, resp) in PROBE_TABLE {
        let mut e = emu();
        assert_eq!(e.feed(probe), resp, "{name}: first probe must be answered");
        assert!(
            e.feed(probe).is_empty(),
            "{name}: an identical retry must be suppressed by the dedup bitmask"
        );
        if let Some(alt) = alt {
            assert!(
                e.feed(alt).is_empty(),
                "{name}: the alternate spelling maps to the same dedup bit and must be suppressed too"
            );
        }
    }
}

#[test]
fn the_five_probe_kinds_dedup_independently() {
    let mut e = emu();
    let mut first_pass = Vec::new();
    for &(_, probe, _, _) in PROBE_TABLE {
        first_pass.extend_from_slice(&e.feed(probe));
    }
    let expected: Vec<u8> = PROBE_TABLE
        .iter()
        .flat_map(|&(_, _, _, resp)| resp.to_vec())
        .collect();
    assert_eq!(
        first_pass, expected,
        "each kind answers once, in table order"
    );

    let second_pass: Vec<u8> = PROBE_TABLE
        .iter()
        .flat_map(|&(_, probe, _, _)| e.feed(probe))
        .collect();
    assert!(
        second_pass.is_empty(),
        "a second sweep of every kind must be fully suppressed by the bitmask"
    );
}

// ── MAX_PROBE_LEN discard ───────────────────────────────────────────────────

#[test]
fn sequence_exceeding_max_probe_len_is_discarded_without_poisoning_state() {
    let mut e = emu();
    // ESC [ + 31 parameter bytes + final = 34 bytes > MAX_PROBE_LEN (32):
    // the accumulator is discarded mid-sequence and nothing is answered,
    // even though the trailing bytes would otherwise complete a probe.
    let oversized: Vec<u8> = b"\x1b["
        .iter()
        .chain(b"0".repeat(31).iter())
        .chain(b"c".iter())
        .copied()
        .collect();
    assert!(
        e.feed(&oversized).is_empty(),
        "an over-cap sequence must be discarded, not answered"
    );
    assert!(
        e.feed(b"c").is_empty(),
        "a stray final byte after a discard is not a probe"
    );
    assert_eq!(
        e.feed(b"\x1b[c"),
        b"\x1b[?6c",
        "the state machine must keep answering after a discard"
    );
}

// ── Unknown sequences ───────────────────────────────────────────────────────

#[test]
fn unknown_csi_sequences_produce_no_response_and_no_error() {
    for seq in UNKNOWN_SEQUENCES {
        let mut e = emu();
        assert!(
            e.feed(seq).is_empty(),
            "{seq:?} must be silently ignored — no response, never an error"
        );
        assert_eq!(
            e.feed(b"\x1b[c"),
            b"\x1b[?6c",
            "the state machine must keep answering after an unknown sequence"
        );
    }
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

// ── Version-pinned capture (claude 2.1.282) ─────────────────────────────────

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).expect("terminal probe fixture must parse")
}

#[test]
fn fixture_pins_claude_version_and_capture_metadata() {
    let f = fixture();
    assert_eq!(
        f["claude_version"].as_str().unwrap_or(""),
        FIXTURE_VERSION,
        "fixture version stamp must match the file's v<version> name — update both together"
    );
    assert!(!f["measured_at"].as_str().unwrap_or_default().is_empty());
    assert_eq!(
        f["truncated"].as_bool(),
        Some(false),
        "the pinned capture must be complete, not clipped at the size cap"
    );
    let isolation = f["isolation"].as_str().unwrap_or_default();
    assert!(
        isolation.contains("--answer"),
        "the pinned capture must be the answered (production) probe shape"
    );
    assert!(
        !f["chunks"].as_array().expect("chunks array").is_empty(),
        "the capture must hold at least one read chunk"
    );
}

#[test]
fn fixture_capture_is_answered_with_exactly_the_documented_responses() {
    let f = fixture();
    // Expected: the capture's probe inventory, deduplicated in capture order,
    // mapped through the doc table.
    let mut expected = Vec::new();
    let mut seen = HashSet::new();
    for inv in f["probe_inventory"].as_array().expect("probe_inventory") {
        let name = inv["name"].as_str().expect("probe name");
        if seen.insert(name) {
            expected.extend_from_slice(response_for(name));
        }
    }

    let mut e = emu();
    let mut got = Vec::new();
    for chunk in f["chunks"].as_array().expect("chunks") {
        got.extend_from_slice(&e.feed(&unesc(chunk.as_str().expect("chunk string"))));
    }

    assert_eq!(
        got, expected,
        "feeding the recorded capture must answer exactly the deduped documented probes — \
         a new TUI probe here means the responder contract moved; re-measure"
    );
    // The pinned shape, spelled out (identical on 2.1.270 and 2.1.282):
    // XTVERSION's DCS string answered first, then DA1 once — the capture's
    // second DA1 is dedup-suppressed.
    assert_eq!(got, b"\x1bP>|claude-print\x1b\\\x1b[?6c");
}

#[test]
fn fixture_capture_fed_byte_by_byte_produces_identical_responses() {
    let f = fixture();
    let chunks: Vec<Vec<u8>> = f["chunks"]
        .as_array()
        .expect("chunks")
        .iter()
        .map(|c| unesc(c.as_str().expect("chunk string")))
        .collect();

    let mut chunked = emu();
    let mut as_recorded = Vec::new();
    for chunk in &chunks {
        as_recorded.extend_from_slice(&chunked.feed(chunk));
    }

    let mut bytewise = emu();
    let mut one_byte_at_a_time = Vec::new();
    for chunk in &chunks {
        for byte in chunk {
            one_byte_at_a_time.extend_from_slice(&bytewise.feed(&[*byte]));
        }
    }

    assert_eq!(
        as_recorded, one_byte_at_a_time,
        "splitting the real capture at every read boundary must not change the answers"
    );
}

#[test]
fn fixture_probe_inventory_is_a_subset_of_the_doc_table() {
    let f = fixture();
    let mut recognized = 0;
    for seq in f["csi_sequences"].as_array().expect("csi_sequences") {
        let name = seq["probe"].as_str().unwrap_or_default();
        if name.is_empty() {
            continue; // unknown sequence — its silence is pinned by the response tests
        }
        recognized += 1;
        assert!(
            PROBE_TABLE.iter().any(|&(kind, ..)| kind == name),
            "probe kind {name} recognized in the capture must be a documented kind"
        );
        // Rebuild the full probe bytes from the recorded params + final and
        // require them to be one of the table's documented spellings.
        let params = unesc(seq["params"].as_str().unwrap_or_default());
        let final_byte = *seq["final"]
            .as_str()
            .expect("final byte string")
            .as_bytes()
            .first()
            .expect("non-empty final byte");
        let rebuilt: Vec<u8> = [b"\x1b[", params.as_slice(), &[final_byte]].concat();
        assert!(
            PROBE_TABLE.iter().any(|&(_, probe, alt, _)| {
                probe == rebuilt.as_slice() || alt.is_some_and(|a| a == rebuilt.as_slice())
            }),
            "probe {name} ({rebuilt:?}) recognized in the capture must match a documented spelling"
        );
    }
    assert!(
        recognized > 0,
        "the pinned capture must contain recognized probes"
    );
}

/// The retained 2.1.270 capture is the compatibility baseline: the recognized
/// probe traffic (kind, spelling, and capture order) must be identical to the
/// pinned 2.1.282 one, which is the executable form of the comparison evidence
/// in docs/notes/terminal-probes.md §Version-Pinned Capture. A failure here
/// means a re-pin changed the responder's real-traffic contract, not just its
/// version stamp — re-measure and update the doc table before re-pinning.
#[test]
fn retained_prior_capture_probe_inventory_matches_the_pinned_shape() {
    let prior: Value = serde_json::from_str(PRIOR_FIXTURE).expect("prior fixture must parse");
    assert_eq!(
        prior["claude_version"].as_str().unwrap_or(""),
        PRIOR_FIXTURE_VERSION,
        "prior fixture version stamp must match its v<version> filename"
    );

    let inventory = |f: &Value| -> Vec<(String, String)> {
        f["probe_inventory"]
            .as_array()
            .expect("probe_inventory")
            .iter()
            .map(|inv| {
                (
                    inv["name"].as_str().expect("probe name").to_string(),
                    inv["params"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    };

    assert_eq!(
        inventory(&prior),
        inventory(&fixture()),
        "the recognized probe inventory (kind, spelling, order) must be identical across the \
         retained 2.1.270 and pinned {FIXTURE_VERSION} captures — XTVERSION once, then DA1 twice"
    );

    // The unknown-sequence set may grow between versions (2.1.282 adds 16t and
    // ?1016$p), but every probe kind recognized in the prior capture must
    // still be recognized — silence can be added, answers cannot be lost.
    let recognized = |f: &Value| -> HashSet<String> {
        f["csi_sequences"]
            .as_array()
            .expect("csi_sequences")
            .iter()
            .filter_map(|s| {
                let name = s["probe"].as_str().unwrap_or_default();
                (!name.is_empty())
                    .then(|| format!("{}:{}", name, s["params"].as_str().unwrap_or_default()))
            })
            .collect()
    };
    let prior_recognized = recognized(&prior);
    let pinned_recognized = recognized(&fixture());
    assert_eq!(
        prior_recognized, pinned_recognized,
        "no recognized probe spelling may appear or disappear between retained and pinned captures"
    );
}

// ── Startup sequencer idle fallback ─────────────────────────────────────────

#[test]
fn startup_idle_fallback_fires_after_the_documented_quiet_window() {
    // docs/notes/terminal-probes.md: no dialog on screen, ≥ 200 bytes received
    // (src/startup.rs IDLE_THRESHOLD_BYTES), then 0.4 s of quiet
    // (IDLE_TIMEOUT_MS) → a bare CR. 200 'x' bytes carry no trust keywords.
    let mut seq = StartupSeq::new(Vec::new());
    assert!(
        matches!(seq.feed(&[b'x'; 200]), StartupAction::None),
        "reaching the byte threshold alone must not fire the fallback"
    );
    // The quiet window starts at the last output chunk — an immediate poll
    // must not fire.
    assert!(matches!(seq.poll_timers(), StartupAction::None));
    std::thread::sleep(Duration::from_millis(500)); // comfortably past 400 ms
    match seq.poll_timers() {
        StartupAction::Write(payload) => assert_eq!(payload, b"\r", "fallback is a bare CR"),
        other => panic!("expected the idle-fallback bare CR, got {other:?}"),
    }
}

#[test]
fn startup_idle_fallback_quiet_window_restarts_on_output() {
    let mut seq = StartupSeq::new(Vec::new());
    seq.feed(&[b'x'; 200]);
    std::thread::sleep(Duration::from_millis(300)); // quiet, but short of the window
    seq.feed(b"more output"); // restarts the quiet window
    assert!(
        matches!(seq.poll_timers(), StartupAction::None),
        "fresh output must restart the quiet window"
    );
    std::thread::sleep(Duration::from_millis(450)); // past 400 ms since the restart
    match seq.poll_timers() {
        StartupAction::Write(payload) => assert_eq!(payload, b"\r", "fallback is a bare CR"),
        other => {
            panic!("expected the idle-fallback bare CR after the restarted window, got {other:?}")
        }
    }
}
