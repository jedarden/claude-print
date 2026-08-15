const PROBE_DA1: u8 = 1 << 0;
const PROBE_DA2: u8 = 1 << 1;
const PROBE_DSR: u8 = 1 << 2;
const PROBE_XTVER: u8 = 1 << 3;
const PROBE_WINSIZE: u8 = 1 << 4;

// Safety margin: no real terminal probe exceeds this length.
const MAX_PROBE_LEN: usize = 32;

enum ParseState {
    Incomplete,
    Invalid,
    Complete,
}

#[derive(Copy, Clone)]
#[allow(clippy::upper_case_acronyms)]
enum ProbeKind {
    DA1,
    DA2,
    DSR,
    XTVersion,
    WinSize,
}

/// Scans PTY output for DEC terminal probes sent by Ink at startup.
///
/// Responds to DA1/DA2/DSR/XTVERSION/window-size; silently ignores unknown
/// sequences. Probes may be split across chunk boundaries — state persists
/// between [`feed`](TerminalEmu::feed) calls. Each probe type is answered at
/// most once per session (dedup bitmask).
pub struct TerminalEmu {
    rows: u16,
    cols: u16,
    /// Partial CSI sequence accumulator; persists across feed() calls.
    partial: Vec<u8>,
    /// Bitmask of probe types already answered this session.
    answered: u8,
}

impl TerminalEmu {
    pub fn new(rows: u16, cols: u16) -> Self {
        TerminalEmu {
            rows,
            cols,
            partial: Vec::new(),
            answered: 0,
        }
    }

    /// Feed a chunk of PTY output. Returns bytes to write back to the PTY master.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;

        while i < chunk.len() {
            let byte = chunk[i];
            i += 1;

            if self.partial.is_empty() {
                if byte == b'\x1b' {
                    self.partial.push(byte);
                }
                continue;
            }

            self.partial.push(byte);

            if self.partial.len() > MAX_PROBE_LEN {
                self.partial.clear();
                continue;
            }

            match self.check_state() {
                ParseState::Complete => {
                    if let Some(resp) = self.emit_response() {
                        out.extend_from_slice(&resp);
                    }
                    self.partial.clear();
                }
                ParseState::Invalid => {
                    // If the byte that invalidated the sequence is itself ESC,
                    // keep it as the start of the next candidate sequence.
                    // SAFETY: We check if the buffer is non-empty before accessing last().
                    // The buffer was just pushed to on line 67, so it's guaranteed non-empty.
                    let last = *self.partial.last().unwrap_or(&byte);
                    self.partial.clear();
                    if last == b'\x1b' {
                        self.partial.push(b'\x1b');
                    }
                }
                ParseState::Incomplete => {}
            }
        }

        out
    }

    fn check_state(&self) -> ParseState {
        let buf = &self.partial;

        // Check buffer length first to avoid panics on empty buffer
        let first = match buf.first() {
            Some(&b) => b,
            None => return ParseState::Incomplete,
        };

        if first != b'\x1b' {
            return ParseState::Invalid;
        }
        if buf.len() == 1 {
            return ParseState::Incomplete;
        }

        let second = buf[1]; // Safe: we know len >= 2
        if second != b'[' {
            return ParseState::Invalid;
        }
        if buf.len() == 2 {
            return ParseState::Incomplete;
        }

        // CSI body: param/intermediate bytes in 0x20-0x3F, final byte in 0x40-0x7E.
        // At this point buf.len() >= 3, so buf.last() always returns Some.
        let last = match buf.last() {
            Some(&b) => b,
            None => return ParseState::Invalid, // Should never happen, but defensive
        };
        if (0x40..=0x7E).contains(&last) {
            ParseState::Complete
        } else if (0x20..=0x3F).contains(&last) {
            ParseState::Incomplete
        } else {
            ParseState::Invalid
        }
    }

    fn identify(&self) -> Option<ProbeKind> {
        // partial = [ESC, '[', params..., final]
        let params = &self.partial[2..];
        if params == b"c" || params == b"0c" {
            Some(ProbeKind::DA1)
        } else if params == b">c" || params == b">0c" {
            Some(ProbeKind::DA2)
        } else if params == b"6n" {
            Some(ProbeKind::DSR)
        } else if params == b">q" || params == b">0q" {
            Some(ProbeKind::XTVersion)
        } else if params == b"18t" {
            Some(ProbeKind::WinSize)
        } else {
            None
        }
    }

    fn emit_response(&mut self) -> Option<Vec<u8>> {
        let kind = self.identify()?;
        let bit = match kind {
            ProbeKind::DA1 => PROBE_DA1,
            ProbeKind::DA2 => PROBE_DA2,
            ProbeKind::DSR => PROBE_DSR,
            ProbeKind::XTVersion => PROBE_XTVER,
            ProbeKind::WinSize => PROBE_WINSIZE,
        };
        if self.answered & bit != 0 {
            return None;
        }
        self.answered |= bit;
        let resp = match kind {
            ProbeKind::DA1 => b"\x1b[?6c".to_vec(),
            ProbeKind::DA2 => b"\x1b[>0;0;0c".to_vec(),
            ProbeKind::DSR => b"\x1b[1;1R".to_vec(),
            // DCS string: ESC P >|claude-print ST  (ST = ESC \)
            ProbeKind::XTVersion => b"\x1bP>|claude-print\x1b\\".to_vec(),
            ProbeKind::WinSize => format!("\x1b[8;{};{}t", self.rows, self.cols).into_bytes(),
        };
        Some(resp)
    }
}
