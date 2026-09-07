use crate::error::{Error, Result};
use crate::util::get_home;
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
///
/// # Errors
///
/// If the transcript path must be derived, returns `Error::Config` when `HOME`
/// is unset, empty, inaccessible, not a directory, or not writable. An explicit
/// transcript path is already authoritative, so direct library calls do not read
/// `HOME` in that branch. The CLI nevertheless validates `HOME` before dispatch.
/// See [`get_home`](crate::util::get_home) for the canonical strict-policy
/// rationale and exact error forms.
pub fn resolve_stop_info(payload: StopPayload) -> Result<StopInfo> {
    let explicit_path = payload
        .transcript_path
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);

    let transcript_path = if explicit_path.is_some() {
        explicit_path
    } else {
        match (&payload.session_id, &payload.cwd) {
            (Some(sid), Some(cwd)) if !sid.is_empty() && !cwd.is_empty() => {
                Some(derive_transcript_path(sid, cwd)?)
            }
            _ => None,
        }
    };

    Ok(StopInfo {
        session_id: payload.session_id,
        transcript_path,
        last_assistant_message: payload.last_assistant_message,
    })
}

/// Build the full transcript path from `session_id` and `cwd`.
///
/// Slug algorithm: claude 2.1.263 folds **every** non-alphanumeric byte of the
/// cwd to `-` — including the leading `/` — so `/home/coding/myproject` → slug
/// `-home-coding-myproject`. Full path:
/// `$HOME/.claude/projects/<slug>/<session_id>.jsonl`
///
/// # Errors
///
/// Returns `Error::Config` if `HOME` is unset, empty, inaccessible, not a
/// directory, or not writable. No fallback directory is attempted. See
/// [`get_home`](crate::util::get_home) for the canonical strict-policy rationale
/// and exact error forms. Invalid `cwd` values also return `Error::Config`.
pub fn derive_transcript_path(session_id: &str, cwd: &str) -> Result<PathBuf> {
    let slug = cwd_to_slug(cwd)?;
    let home = get_home()?;
    Ok(home
        .join(".claude")
        .join("projects")
        .join(&slug)
        .join(format!("{session_id}.jsonl")))
}

/// Convert a filesystem `cwd` path to a JSONL directory slug, mirroring the
/// scheme claude 2.1.263 actually uses for `~/.claude/projects/`.
///
/// claude folds **every** character outside `[a-zA-Z0-9]` to `-` — the leading
/// `/` of an absolute path becomes a leading dash, and `_`/`.` and other
/// punctuation fold too:
///
/// ```text
/// /home/coding/claude-print        → -home-coding-claude-print
/// /tmp/probe-B_no_marker-1788799902 → -tmp-probe-B-no-marker-1788799902
/// ```
///
/// (Both verified against live `~/.claude/projects/` contents; the earlier
/// strip-leading-slash/split-on-slash scheme produced slugs claude never
/// creates, which silently broke every derived transcript path and the
/// stream-json discovery directory — bead claudepr-26e7a0b6.)
///
/// The result can only contain alphanumerics and dashes, so no component
/// validation is needed: traversal sequences fold to dashes, never survive.
/// Slugs longer than 200 characters are truncated to claude's cap — claude
/// additionally appends a hash of the original path in that regime, which is
/// not reproduced here (paths that deep are pathological, and Stop payloads
/// carrying an explicit `transcript_path` never consult this derivation).
///
/// # Errors
/// Returns `Error::Config` if the path contains a null byte or is empty.
pub fn cwd_to_slug(cwd: &str) -> Result<String> {
    // Null bytes cannot appear in claude's slug (they fold) but they also make
    // the input unusable as a path; reject rather than silently fold.
    if cwd.contains('\0') {
        return Err(Error::Config("path contains null byte".to_string()));
    }

    /// claude caps the folded slug at 200 characters before any hash suffix.
    const MAX_SLUG_LEN: usize = 200;

    // Every folded character is ASCII by construction, so truncation below
    // cannot split one.
    let mut slug: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    if slug.is_empty() {
        return Err(Error::Config("path is empty".to_string()));
    }

    slug.truncate(MAX_SLUG_LEN);
    Ok(slug)
}

/// The projects directory claude writes session transcripts under, derived from
/// the current working directory: `$HOME/.claude/projects/<cwd-slug>/`.
///
/// Used at `PROMPT_INJECTED` to point the live stream-json reader at the
/// directory it must DISCOVER this session's `<session_id>.jsonl` in — the
/// `session_id` is unknown until the Stop payload arrives, after injection.
///
/// # Errors
///
/// Returns `Error::Config` if `HOME` is unset, empty, inaccessible, not a
/// directory, or not writable. No fallback directory is attempted. See
/// [`get_home`](crate::util::get_home) for the canonical strict-policy rationale
/// and exact error forms. Invalid working-directory values also return
/// `Error::Config`; failure to read the current directory returns `Error::Io`.
pub fn projects_dir_for_cwd() -> Result<PathBuf> {
    let cwd = std::env::current_dir().map_err(Error::Io)?;
    let slug = cwd_to_slug(&cwd.to_string_lossy())?;
    let home = get_home()?;
    Ok(home.join(".claude").join("projects").join(slug))
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

    // ── cwd_to_slug (claude 2.1.263 scheme: fold every non-alphanumeric) ──────

    #[test]
    fn cwd_to_slug_matches_claude_project_dir_for_this_repo() {
        // Verified live: /home/coding/claude-print's real transcript dir is
        // ~/.claude/projects/-home-coding-claude-print/ (leading dash).
        assert_eq!(
            cwd_to_slug("/home/coding/claude-print").unwrap(),
            "-home-coding-claude-print"
        );
    }

    #[test]
    fn cwd_to_slug_folds_underscores_like_claude() {
        // Verified live: a session with cwd /tmp/probe-B_no_marker-1788799902
        // wrote its transcript under -tmp-probe-B-no-marker-1788799902/.
        assert_eq!(
            cwd_to_slug("/tmp/probe-B_no_marker-1788799902").unwrap(),
            "-tmp-probe-B-no-marker-1788799902"
        );
    }

    #[test]
    fn cwd_to_slug_tradegraph_vector() {
        // Third live-verified vector: existing projects dir -home-coding-tradegraph-platform.
        assert_eq!(
            cwd_to_slug("/home/coding/tradegraph-platform").unwrap(),
            "-home-coding-tradegraph-platform"
        );
    }

    #[test]
    fn cwd_to_slug_relative_path_has_no_leading_dash() {
        assert_eq!(cwd_to_slug("tmp/foo").unwrap(), "tmp-foo");
    }

    #[test]
    fn cwd_to_slug_rejects_null_bytes() {
        let result = cwd_to_slug("/home/coding/\0project");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("null byte"));
    }

    #[test]
    fn cwd_to_slug_folds_control_characters() {
        // Control characters fold to '-' exactly like any other non-alphanumeric,
        // mirroring claude's [^a-zA-Z0-9] fold. Nothing survives to be dangerous.
        assert_eq!(cwd_to_slug("/home/cod\x01ing").unwrap(), "-home-cod-ing");
        assert_eq!(
            cwd_to_slug("/home/coding\n/project").unwrap(),
            "-home-coding--project"
        );
        assert_eq!(
            cwd_to_slug("/home/coding\r/project").unwrap(),
            "-home-coding--project"
        );
        assert_eq!(
            cwd_to_slug("/home/coding\tproject").unwrap(),
            "-home-coding-project"
        );
    }

    #[test]
    fn cwd_to_slug_folds_traversal_components_to_dashes() {
        // '.' and '..' are not special after the fold — the result is a single
        // flat directory NAME containing only alphanumerics and dashes, so
        // traversal cannot survive.
        assert_eq!(
            cwd_to_slug("/home/./coding/project").unwrap(),
            "-home---coding-project"
        );
        assert_eq!(
            cwd_to_slug("/home/../etc/passwd").unwrap(),
            "-home----etc-passwd"
        );
    }

    #[test]
    fn cwd_to_slug_caps_at_claude_200_char_limit() {
        // claude folds, then caps the slug at 200 chars (appending a hash of the
        // original path that claude-print does not reproduce). The 200-char cap
        // also keeps the single-component name within filesystem NAME_MAX (255).
        let long = format!("/home/{}", "a".repeat(300));
        let slug = cwd_to_slug(&long).unwrap();
        assert_eq!(slug.len(), 200);
        assert!(slug.starts_with("-home-"));
        assert!(slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    #[test]
    fn cwd_to_slug_preserves_consecutive_and_trailing_slashes_as_dashes() {
        // Dashes are NOT collapsed — /home//x folds with two dashes, mirroring
        // the double dashes observed in real projects dir names.
        assert_eq!(
            cwd_to_slug("/home//coding/project").unwrap(),
            "-home--coding-project"
        );
        assert_eq!(
            cwd_to_slug("/home/coding/project/").unwrap(),
            "-home-coding-project-"
        );
        assert_eq!(cwd_to_slug("///").unwrap(), "---");
    }

    #[test]
    fn cwd_to_slug_rejects_empty_path() {
        let result = cwd_to_slug("");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn cwd_to_slug_root_folds_to_single_dash() {
        // cwd "/" folds to "-" — a valid (if unusual) directory name, exactly
        // what claude's own fold produces.
        assert_eq!(cwd_to_slug("/").unwrap(), "-");
    }

    #[test]
    fn cwd_to_slug_folds_unicode_to_dashes() {
        // claude's fold is ASCII-only: non-ASCII characters become dashes.
        assert_eq!(
            cwd_to_slug("/home/coding/projet-тест").unwrap(),
            "-home-coding-projet-----"
        );
    }

    #[test]
    fn cwd_to_slug_valid_multi_component_path() {
        assert_eq!(
            cwd_to_slug("/usr/local/bin/project").unwrap(),
            "-usr-local-bin-project"
        );
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
        let info = resolve_stop_info(payload).unwrap();
        assert_eq!(
            info.transcript_path,
            Some(PathBuf::from("/explicit/path.jsonl"))
        );
    }

    #[test]
    fn resolve_derives_path_when_transcript_path_absent() {
        // Direct mutation selects the derivation branch's strict HOME behavior;
        // the original value is restored below.
        let original_home = std::env::var("HOME").ok();

        let home_dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home_dir.path());

        let payload = StopPayload {
            session_id: Some("mysession".to_string()),
            transcript_path: None,
            cwd: Some("/home/user/myproject".to_string()),
            last_assistant_message: None,
        };
        let info = resolve_stop_info(payload).unwrap();
        let expected = home_dir
            .path()
            .join(".claude")
            .join("projects")
            .join("-home-user-myproject")
            .join("mysession.jsonl");
        assert_eq!(info.transcript_path, Some(expected));

        // Restore environment
        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn resolve_returns_none_when_no_derivation_possible() {
        let payload = StopPayload {
            session_id: Some("sid".to_string()),
            transcript_path: None,
            cwd: None, // cwd absent: cannot derive
            last_assistant_message: None,
        };
        let info = resolve_stop_info(payload).unwrap();
        assert!(info.transcript_path.is_none());
    }

    #[test]
    fn resolve_fails_when_home_not_set() {
        // Save original HOME
        let original_home = std::env::var("HOME").ok();

        // Unset HOME
        std::env::remove_var("HOME");

        let payload = StopPayload {
            session_id: Some("sid".to_string()),
            transcript_path: None,
            cwd: Some("/home/user/myproject".to_string()),
            last_assistant_message: None,
        };

        let result = resolve_stop_info(payload);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("HOME environment variable not set"));

        // Restore HOME
        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        }
    }

    #[test]
    fn derive_transcript_path_fails_when_home_not_set() {
        // Save original HOME
        let original_home = std::env::var("HOME").ok();

        // Unset HOME
        std::env::remove_var("HOME");

        let result = derive_transcript_path("sid123", "/home/user/project");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("HOME environment variable not set"));

        // Restore HOME
        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        }
    }

    #[test]
    fn projects_dir_for_cwd_fails_when_home_not_set() {
        // Save original HOME
        let original_home = std::env::var("HOME").ok();

        // Unset HOME
        std::env::remove_var("HOME");

        let result = projects_dir_for_cwd();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("HOME environment variable not set"));

        // Restore HOME
        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        }
    }

    #[test]
    fn derive_transcript_path_builds_correct_path() {
        // Direct mutation supplies a deterministic HOME and is restored below.
        let original_home = std::env::var("HOME").ok();
        let home_dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home_dir.path());
        let result = derive_transcript_path("sess-id", "/project/dir");
        assert!(result.is_ok());
        let path = result.unwrap();
        assert_eq!(
            path,
            home_dir
                .path()
                .join(".claude/projects/-project-dir/sess-id.jsonl")
        );
        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn projects_dir_for_cwd_builds_correct_path() {
        // Direct mutation supplies a deterministic HOME and is restored below.
        let original_home = std::env::var("HOME").ok();
        let home_dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home_dir.path());
        let result = projects_dir_for_cwd();
        assert!(result.is_ok());
        let path = result.unwrap();
        // The actual current directory should be used, not PWD env var
        let cwd = std::env::current_dir().expect("current_dir");
        let cwd_slug = cwd_to_slug(&cwd.to_string_lossy()).expect("cwd_to_slug");
        assert_eq!(
            path,
            home_dir.path().join(".claude/projects").join(cwd_slug)
        );
        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
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
