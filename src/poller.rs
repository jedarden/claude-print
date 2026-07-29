use crate::error::{Error, Result};
use serde::Deserialize;
use std::os::unix::io::{FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

/// Raw Stop hook payload received from Claude Code via the FIFO.
/// All fields are optional for forward compatibility with future schema changes.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct StopPayload {
    pub session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub last_assistant_message: Option<String>,
    pub cwd: Option<String>,
}

/// Resolved stop information after transcript path derivation.
#[derive(Debug)]
pub struct StopInfo {
    pub session_id: Option<String>,
    /// Resolved transcript path: from payload if present, otherwise derived from
    /// session_id + cwd.  `None` if neither derivation is possible.
    pub transcript_path: Option<PathBuf>,
    pub last_assistant_message: Option<String>,
}

/// Parse raw FIFO bytes into a [`StopPayload`].
///
/// Finds the first non-empty line and decodes it as JSON.  Unknown fields are
/// silently ignored (`#[serde(default)]` + no `deny_unknown_fields`).
pub fn parse_stop_payload(bytes: &[u8]) -> Result<StopPayload> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| Error::Internal(anyhow::anyhow!("stop payload not UTF-8: {e}")))?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        return serde_json::from_str(line)
            .map_err(|e| Error::Internal(anyhow::anyhow!("stop payload JSON parse failed: {e}")));
    }
    Ok(StopPayload::default())
}

/// Resolve a [`StopPayload`] into [`StopInfo`], deriving the transcript path
/// when `transcript_path` is absent but `session_id` and `cwd` are present.
pub fn resolve_stop_info(payload: StopPayload) -> StopInfo {
    let explicit_path = payload
        .transcript_path
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);

    let transcript_path = explicit_path.or_else(|| match (&payload.session_id, &payload.cwd) {
        (Some(sid), Some(cwd)) if !sid.is_empty() && !cwd.is_empty() => {
            Some(derive_transcript_path(sid, cwd))
        }
        _ => None,
    });

    StopInfo {
        session_id: payload.session_id,
        transcript_path,
        last_assistant_message: payload.last_assistant_message,
    }
}

/// Build the full transcript path from `session_id` and `cwd`.
///
/// Slug algorithm: strip the leading `/` from `cwd`, replace remaining `/` with `-`.
/// Example: `/home/coding/myproject` → slug `home-coding-myproject`
/// Full path: `$HOME/.claude/projects/<slug>/<session_id>.jsonl`
pub fn derive_transcript_path(session_id: &str, cwd: &str) -> PathBuf {
    let slug = cwd_to_slug(cwd);
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home)
        .join(".claude")
        .join("projects")
        .join(&slug)
        .join(format!("{session_id}.jsonl"))
}

/// Convert a filesystem `cwd` path to a JSONL directory slug.
///
/// Strip the leading `/`, then replace all `/` with `-`.
pub fn cwd_to_slug(cwd: &str) -> String {
    cwd.trim_start_matches('/').replace('/', "-")
}

/// The projects directory claude writes session transcripts under, derived from
/// the current working directory: `$HOME/.claude/projects/<cwd-slug>/`.
///
/// Used at `PROMPT_INJECTED` to point the live stream-json reader at the
/// directory it must DISCOVER this session's `<session_id>.jsonl` in — the
/// `session_id` is unknown until the Stop payload arrives, after injection.
/// Returns `None` only if the cwd cannot be read, in which case the reader is
/// not spawned (live tailing disabled for that run).
pub fn projects_dir_for_cwd() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let slug = cwd_to_slug(&cwd.to_string_lossy());
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    Some(
        PathBuf::from(home)
            .join(".claude")
            .join("projects")
            .join(slug),
    )
}

/// Open the named FIFO at `path` for non-blocking reading.
///
/// Linux FIFO O_NONBLOCK semantics:
/// - `O_RDONLY|O_NONBLOCK`: always succeeds immediately (no writer required).
/// - `O_WRONLY|O_NONBLOCK`: returns `ENXIO` if no reader is present.
///
/// We therefore open the **read-end first** (always succeeds), then open a
/// "keeper" write-end `O_WRONLY|O_NONBLOCK` which now succeeds because the
/// read-end is already open.  The keeper is held open until the Stop hook fires
/// so that the hook's `cat > fifo` can open a write-end without getting
/// `ENXIO`.  Closing the keeper after the payload is read causes any lingering
/// `cat > fifo` in hook.sh to receive `EPIPE`/`ENXIO` and exit cleanly.
///
/// Returns `(read_fd, keeper_write_fd)`.
pub fn open_fifo_nonblock(path: &Path) -> Result<(OwnedFd, OwnedFd)> {
    use std::os::unix::ffi::OsStrExt;

    let path_cstr = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| Error::Internal(anyhow::anyhow!("FIFO path has null byte: {e}")))?;

    // Open read-end first: O_RDONLY|O_NONBLOCK never fails with ENXIO.
    let read_fd = unsafe {
        libc::open(
            path_cstr.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if read_fd < 0 {
        let e = nix::errno::Errno::last();
        return Err(Error::Internal(anyhow::anyhow!(
            "open FIFO read-end failed: {e}"
        )));
    }
    let read_fd = unsafe { OwnedFd::from_raw_fd(read_fd) };

    // Open keeper write-end: succeeds because the read-end is now open.
    let write_fd = unsafe {
        libc::open(
            path_cstr.as_ptr(),
            libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if write_fd < 0 {
        let e = nix::errno::Errno::last();
        return Err(Error::Internal(anyhow::anyhow!(
            "open FIFO write-end (keeper) failed: {e}"
        )));
    }
    let write_fd = unsafe { OwnedFd::from_raw_fd(write_fd) };

    Ok((read_fd, write_fd))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── cwd_to_slug ───────────────────────────────────────────────────────────

    #[test]
    fn cwd_to_slug_home_coding_myproject() {
        assert_eq!(
            cwd_to_slug("/home/coding/myproject"),
            "home-coding-myproject"
        );
    }

    #[test]
    fn cwd_to_slug_root_foo_bar() {
        assert_eq!(cwd_to_slug("/root/foo/bar"), "root-foo-bar");
    }

    #[test]
    fn cwd_to_slug_tmp() {
        assert_eq!(cwd_to_slug("/tmp"), "tmp");
    }

    #[test]
    fn cwd_to_slug_tmp_x() {
        assert_eq!(cwd_to_slug("/tmp/x"), "tmp-x");
    }

    #[test]
    fn cwd_to_slug_no_leading_slash() {
        assert_eq!(cwd_to_slug("tmp/foo"), "tmp-foo");
    }

    // ── parse_stop_payload ────────────────────────────────────────────────────

    #[test]
    fn parse_full_payload() {
        let json = r#"{"hook_event_name":"Stop","session_id":"abc-123","transcript_path":"/home/u/.claude/projects/foo/abc-123.jsonl","cwd":"/home/u/foo","last_assistant_message":"hello"}"#;
        let p = parse_stop_payload(json.as_bytes()).unwrap();
        assert_eq!(p.session_id.as_deref(), Some("abc-123"));
        assert_eq!(
            p.transcript_path.as_deref(),
            Some("/home/u/.claude/projects/foo/abc-123.jsonl")
        );
        assert_eq!(p.cwd.as_deref(), Some("/home/u/foo"));
        assert_eq!(p.last_assistant_message.as_deref(), Some("hello"));
    }

    #[test]
    fn parse_payload_missing_transcript_path() {
        let json = r#"{"hook_event_name":"Stop","session_id":"s1","cwd":"/tmp/foo"}"#;
        let p = parse_stop_payload(json.as_bytes()).unwrap();
        assert!(p.transcript_path.is_none());
        assert_eq!(p.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn parse_payload_unknown_fields_ignored() {
        let json =
            r#"{"hook_event_name":"Stop","session_id":"x","future_field":42,"nested":{"a":1}}"#;
        let p = parse_stop_payload(json.as_bytes()).unwrap();
        assert_eq!(p.session_id.as_deref(), Some("x"));
    }

    #[test]
    fn parse_payload_empty_bytes_returns_default() {
        let p = parse_stop_payload(b"").unwrap();
        assert!(p.session_id.is_none());
        assert!(p.transcript_path.is_none());
    }

    #[test]
    fn parse_payload_trailing_newline() {
        let json = b"{\"session_id\":\"s2\"}\n";
        let p = parse_stop_payload(json).unwrap();
        assert_eq!(p.session_id.as_deref(), Some("s2"));
    }

    #[test]
    fn parse_payload_malformed_json_returns_err() {
        let result = parse_stop_payload(b"not json");
        assert!(result.is_err());
    }

    // ── resolve_stop_info ─────────────────────────────────────────────────────

    #[test]
    fn resolve_uses_explicit_transcript_path() {
        let payload = StopPayload {
            session_id: Some("sid".to_string()),
            transcript_path: Some("/explicit/path.jsonl".to_string()),
            cwd: Some("/some/cwd".to_string()),
            last_assistant_message: None,
        };
        let info = resolve_stop_info(payload);
        assert_eq!(
            info.transcript_path,
            Some(PathBuf::from("/explicit/path.jsonl"))
        );
    }

    #[test]
    fn resolve_derives_path_when_transcript_path_absent() {
        let payload = StopPayload {
            session_id: Some("mysession".to_string()),
            transcript_path: None,
            cwd: Some("/home/user/myproject".to_string()),
            last_assistant_message: None,
        };
        let info = resolve_stop_info(payload);
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let expected = PathBuf::from(&home)
            .join(".claude")
            .join("projects")
            .join("home-user-myproject")
            .join("mysession.jsonl");
        assert_eq!(info.transcript_path, Some(expected));
    }

    #[test]
    fn resolve_returns_none_when_no_derivation_possible() {
        let payload = StopPayload {
            session_id: Some("sid".to_string()),
            transcript_path: None,
            cwd: None, // cwd absent: cannot derive
            last_assistant_message: None,
        };
        let info = resolve_stop_info(payload);
        assert!(info.transcript_path.is_none());
    }

    // ── open_fifo_nonblock (OQ-4: FIFO open race) ─────────────────────────────

    #[test]
    fn open_fifo_nonblock_succeeds_without_separate_writer() {
        use crate::hook::HookInstaller;
        let installer = HookInstaller::new().unwrap();
        // open_fifo_nonblock opens keeper write-end then read-end; must not fail.
        let result = open_fifo_nonblock(&installer.fifo_path);
        assert!(
            result.is_ok(),
            "open_fifo_nonblock must succeed without a pre-existing writer: {:?}",
            result.err()
        );
    }

    #[test]
    fn open_fifo_nonblock_read_end_is_ready_for_poll() {
        use crate::hook::HookInstaller;
        use std::io::Write;
        use std::os::unix::io::AsRawFd;

        let installer = HookInstaller::new().unwrap();
        let (read_fd, _keeper) = open_fifo_nonblock(&installer.fifo_path).unwrap();

        // Write some bytes from a thread (will unblock immediately since keeper write-end is open)
        let fifo_path = installer.fifo_path.clone();
        let writer = std::thread::spawn(move || {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .open(&fifo_path)
                .unwrap();
            f.write_all(b"hello").unwrap();
        });

        // poll() with a short timeout; POLLIN must fire
        let mut pfd = libc::pollfd {
            fd: read_fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ret = unsafe { libc::poll(&mut pfd, 1, 2000) }; // 2 s timeout
        writer.join().unwrap();

        assert!(ret > 0, "poll timed out waiting for FIFO data");
        assert!(pfd.revents & libc::POLLIN != 0, "POLLIN not set");
    }
}
