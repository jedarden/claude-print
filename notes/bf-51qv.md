# bf-51qv — EC-4 null-byte rejection + T-2 --input-file resolve/size-cap

## Task
Implement the two unimplemented plan edge cases:
- **EC-4** — prompt containing an embedded NUL byte is rejected at CLI validation
  time with exit 2 (any source: positional, stdin, `--input-file`), since `claude -p`
  does not support null bytes.
- **T-2** — `--input-file` is resolved to an absolute path and size-checked *before*
  its contents are slurped (prompt-injection / memory-exhaustion hardening).

## Outcome on dispatch
The substantive fix was **already implemented and committed** by a prior run as
`6541da5` ("feat(prompt): EC-4 null-byte rejection + T-2 --input-file
resolve/size-cap (bf-51qv)"), and that commit is **already on `origin/main`**
(verified: `git branch -r --contains 6541da5` includes `origin/main`). The bead
itself was still `in_progress` / never closed, so this re-dispatch's job was to
verify the Definition of Done and close it.

## What the committed fix contains
- **New module `src/prompt.rs`** (253 lines):
  - `find_null_byte(&[u8]) -> Option<usize>` — offset of first NUL (EC-4).
  - `resolve_input_file(&Path) -> Result<PathBuf, InputFileError>` —
    `canonicalize` to an absolute path, `metadata().is_file()` type-check, and a
    `metadata().len() > PROMPT_MAX_BYTES` size-check, all *before* the caller reads
    the contents (T-2). `PROMPT_MAX_BYTES = 10 MiB`, deliberately above the 32 KB
    inline-paste/file-relay threshold so legitimate >32 KB prompts still work.
  - `InputFileError { TooLarge → exit 2, NotRegularFile|ResolveFailed → exit 4 }`
    (exit 4 matches the existing unreadable-`--input-file` convention).
- **`src/main.rs`** wires it in:
  - `--input-file` branch resolves via `resolve_input_file` before `std::fs::read`,
    mapping `TooLarge`→exit 2 and the rest→exit 4.
  - After `prompt_bytes` is resolved from any of the three sources, a single
    `find_null_byte` guard rejects with exit 2 (EC-4).
- **`src/lib.rs`** declares the new `prompt` module.
- **11 unit tests** in `src/prompt.rs` covering all four task-required cases:
  null byte in positional-like bytes, null byte in stdin-like bytes, null byte in
  `--input-file` contents (detected after read), and relative-path resolution that
  reads back correctly (regression). Plus boundary cases (size cap ±1, non-regular
  file, missing path, leading/trailing NUL).

## Definition of Done — re-verified this dispatch
| Gate | Result |
|------|--------|
| `cargo fmt --check` | ✅ clean |
| `cargo clippy --all-targets -- -D warnings` | ✅ exit 0, no warnings |
| `cargo test --lib prompt::` | ✅ 11 passed, 0 failed |
| `cargo test --test cli` | ✅ 23 passed, 0 failed (incl. existing `cli_input_file_flag` relative-path regression) |

## Result
Fix is complete, on `origin/main` (`6541da5`), and all DoD gates pass. This run
produced no new source changes — this notes file is the commit artifact for the
bead, per the dispatch instructions.
