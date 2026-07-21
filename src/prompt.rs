//! Prompt input validation (plan Security > T-2, Edge Cases > EC-4).
//!
//! `claude -p` does not support embedded NUL bytes in a prompt, so the bytes
//! resolved from any prompt source (positional argument, stdin, `--input-file`)
//! are scanned for a NUL byte and rejected at CLI validation time with exit 2
//! (EC-4). For `--input-file` specifically, the path is resolved to an absolute
//! path (resolving symlinks and relative components) and is size-checked *and*
//! type-checked before its contents are slurped, so a caller pointing the flag
//! at `/dev/zero`, a FIFO, or an enormous file cannot exhaust memory (T-2).
//!
//! [`PROMPT_MAX_BYTES`] is deliberately far above the 32 KB inline-paste /
//! file-relay threshold in [`crate::startup`] (`INLINE_PROMPT_MAX`): prompts
//! above 32 KB are a legitimate, supported input (relayed to a temp file), so
//! the cap only bounds the worst case rather than restricting normal use.

use std::io;
use std::path::{Path, PathBuf};

/// Hard ceiling on a single prompt's size, applied to `--input-file` *before*
/// the contents are read (T-2). Generous enough for any legitimate prompt —
/// including the >32 KB file-relay path in [`crate::startup`] — while bounding
/// a single invocation's peak memory against a `/dev/zero`-style or huge-file
/// caller.
pub const PROMPT_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Error raised while resolving `--input-file` to a readable absolute path
/// (T-2). The variant distinguishes a policy rejection (exit 2) from an
/// unreadability failure (exit 4, matching the existing read-error path and
/// the plan's exit-code table row for "unreadable `--input-file`").
#[derive(Debug)]
pub enum InputFileError {
    /// File size exceeded [`PROMPT_MAX_BYTES`]; detected via `metadata()`
    /// before the contents are read. Maps to exit 2 (T-2 policy rejection).
    TooLarge {
        resolved: PathBuf,
        size: u64,
        limit: u64,
    },
    /// Path is not a regular file (directory, FIFO, char/block device, socket,
    /// ...). A non-regular file has no meaningful size — `metadata().len()`
    /// reports 0 for e.g. `/dev/zero`, so the size check alone would not stop
    /// an unbounded `read`. Maps to exit 4 (unreadable `--input-file`).
    NotRegularFile { resolved: PathBuf },
    /// Path could not be canonicalized to an absolute path, or `metadata()`
    /// failed (missing path, broken symlink, permission denied, ...). Maps to
    /// exit 4 (unreadable `--input-file`).
    ResolveFailed { path: PathBuf, source: io::Error },
}

/// Resolve `--input-file` to an absolute path, type-check it, and size-check
/// it *before* the caller slurps the contents (T-2).
///
/// On success returns the canonicalized absolute path for the caller to read.
/// On failure returns [`InputFileError`]; the caller maps [`InputFileError::TooLarge`]
/// to exit 2 and the remaining variants to exit 4.
///
/// The size and type checks happen before the read so that an over-large or
/// non-regular file (e.g. `/dev/zero`) is rejected without ever being copied
/// into memory.
pub fn resolve_input_file(path: &Path) -> Result<PathBuf, InputFileError> {
    let resolved = std::fs::canonicalize(path).map_err(|source| InputFileError::ResolveFailed {
        path: path.to_path_buf(),
        source,
    })?;

    let metadata =
        std::fs::metadata(&resolved).map_err(|source| InputFileError::ResolveFailed {
            path: path.to_path_buf(),
            source,
        })?;

    if !metadata.is_file() {
        return Err(InputFileError::NotRegularFile { resolved });
    }

    let size = metadata.len();
    if size > PROMPT_MAX_BYTES {
        return Err(InputFileError::TooLarge {
            resolved,
            size,
            limit: PROMPT_MAX_BYTES,
        });
    }

    Ok(resolved)
}

/// EC-4: returns the byte offset of the first embedded NUL byte, or `None` if
/// the prompt contains none. Applied to the final bytes resolved from every
/// prompt source (positional, stdin, `--input-file`); a `Some(offset)` result
/// is mapped to exit 2 by the caller.
pub fn find_null_byte(bytes: &[u8]) -> Option<usize> {
    bytes.iter().position(|&b| b == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Monotonic counter for unique in-CWD test filenames so parallel tests do
    /// not collide when exercising relative-path resolution.
    static REL_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// RAII guard that removes a file when dropped, ignoring errors.
    struct RemoveOnDrop(PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    /// Create a uniquely-named file in the process CWD and return its *relative*
    /// path plus a drop guard that cleans it up.
    fn relative_temp_file(contents: &[u8]) -> (PathBuf, RemoveOnDrop) {
        let n = REL_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let name = format!(".cp_prompt_test_{}_{}.tmp", pid, n);
        let rel = PathBuf::from(&name);
        fs::write(&rel, contents).expect("write relative test file");
        let guard = RemoveOnDrop(rel.clone());
        (rel, guard)
    }

    // ── find_null_byte (EC-4) ────────────────────────────────────────────────

    #[test]
    fn null_byte_absent_returns_none() {
        // Mirrors a clean positional/stdin/file prompt.
        assert_eq!(find_null_byte(b"hello world"), None);
        assert_eq!(find_null_byte(b""), None);
        assert_eq!(find_null_byte("multi\nline\nprompt".as_bytes()), None);
    }

    #[test]
    fn null_byte_in_positional_like_prompt_detected() {
        // Bytes as they would arrive from the positional <prompt> argument
        // (prompt_str.as_bytes().to_vec()).
        let bytes = b"be\0fore".to_vec();
        assert_eq!(find_null_byte(&bytes), Some(2));
    }

    #[test]
    fn null_byte_in_stdin_like_prompt_detected() {
        // Bytes as they would arrive from io::stdin().read_to_end().
        let bytes: Vec<u8> = vec![b'x', b'y', 0, b'z'];
        assert_eq!(find_null_byte(&bytes), Some(2));
    }

    #[test]
    fn null_byte_leading_and_trailing() {
        assert_eq!(find_null_byte(&[0u8, b'a', b'b']), Some(0));
        assert_eq!(find_null_byte(&[b'a', b'b', 0]), Some(2));
    }

    // ── resolve_input_file (T-2) ─────────────────────────────────────────────

    #[test]
    fn input_file_relative_path_resolves_to_absolute_and_reads_back() {
        // Regression check (task): canonicalization must not break a relative
        // --input-file. The resolved path must be absolute and read back the
        // original contents.
        let contents = b"relative prompt body";
        let (rel, _guard) = relative_temp_file(contents);

        let resolved = resolve_input_file(&rel).expect("relative path must resolve");
        assert!(
            resolved.is_absolute(),
            "resolved path must be absolute; got {:?}",
            resolved
        );

        let read_back = fs::read(&resolved).expect("read resolved path");
        assert_eq!(read_back, contents);
    }

    #[test]
    fn input_file_absolute_path_resolves_and_reads_back() {
        let tf = tempfile::NamedTempFile::new().expect("create temp file");
        fs::write(tf.path(), b"absolute body").expect("write");
        let resolved = resolve_input_file(tf.path()).expect("absolute path resolves");
        assert!(resolved.is_absolute());
        assert_eq!(fs::read(&resolved).unwrap(), b"absolute body");
    }

    #[test]
    fn input_file_missing_path_is_unreadable() {
        // A path that does not exist cannot be canonicalized → ResolveFailed
        // (caller maps to exit 4, the existing unreadable-`--input-file` code).
        let missing = PathBuf::from("/nonexistent/claude-print-prompt-test-51qv");
        match resolve_input_file(&missing) {
            Err(InputFileError::ResolveFailed { path, .. }) => {
                assert_eq!(path, missing, "should report the original path");
            }
            other => panic!("expected ResolveFailed for missing path, got {:?}", other),
        }
    }

    #[test]
    fn input_file_non_regular_is_rejected() {
        // A directory is not a regular file → NotRegularFile (exit 4). This also
        // covers the /dev/zero class: non-regular files report size 0 and would
        // bypass the size check, so they are rejected by type here.
        let dir = tempfile::TempDir::new().expect("create temp dir");
        match resolve_input_file(dir.path()) {
            Err(InputFileError::NotRegularFile { resolved }) => {
                assert_eq!(resolved, std::fs::canonicalize(dir.path()).unwrap());
            }
            other => panic!("expected NotRegularFile for a directory, got {:?}", other),
        }
    }

    #[test]
    fn input_file_over_size_cap_is_rejected() {
        // PROMPT_MAX_BYTES + 1 must trip the size check (T-2) → TooLarge
        // (caller maps to exit 2) — without ever reading the full contents.
        let tf = tempfile::NamedTempFile::new().expect("create temp file");
        let oversized = vec![b'A'; (PROMPT_MAX_BYTES + 1) as usize];
        fs::write(tf.path(), &oversized).expect("write oversized file");

        match resolve_input_file(tf.path()) {
            Err(InputFileError::TooLarge { size, limit, .. }) => {
                assert_eq!(limit, PROMPT_MAX_BYTES);
                assert_eq!(size, PROMPT_MAX_BYTES + 1);
            }
            other => panic!("expected TooLarge, got {:?}", other),
        }
    }

    #[test]
    fn input_file_at_size_cap_is_accepted() {
        // Exactly PROMPT_MAX_BYTES is the boundary (size > limit is the rule).
        let tf = tempfile::NamedTempFile::new().expect("create temp file");
        let at_cap = vec![b'B'; PROMPT_MAX_BYTES as usize];
        fs::write(tf.path(), &at_cap).expect("write at-cap file");
        resolve_input_file(tf.path()).expect("file at the cap must be accepted");
    }

    #[test]
    fn input_file_contents_with_null_byte_detected_after_read() {
        // End-to-end for the --input-file null-byte path (EC-4): resolution
        // succeeds (the file is a valid regular file), the read returns the
        // bytes, and find_null_byte then flags the embedded NUL → caller exits 2.
        let tf = tempfile::NamedTempFile::new().expect("create temp file");
        fs::write(tf.path(), b"hello\0world").expect("write nul-containing file");

        let resolved = resolve_input_file(tf.path()).expect("resolves");
        let bytes = fs::read(&resolved).expect("read");
        assert_eq!(find_null_byte(&bytes), Some(5));
    }
}
