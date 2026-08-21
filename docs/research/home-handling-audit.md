# HOME Environment Variable Handling Audit

**Audited:** 2026-08-20
**Scope:** Production HOME reads and path-resolution call sites

## Finding

The implementation consistently uses the strict resolver in
`src/util.rs::get_home()`. It returns `Error::Config` when HOME is unset or
empty and never substitutes `/root` or another guessed directory.

The helper uses `std::env::var_os`, so a non-empty non-UTF-8 Unix path is
preserved. It intentionally performs no existence or canonicalization check;
filesystem operations report their own errors later.

## Call-site audit

| Module | Function | Result when HOME is unset or empty |
| --- | --- | --- |
| `util.rs` | `get_home()` | `Error::Config` with an actionable message |
| `config.rs` | `Config::default_path()` | Same error if `XDG_CONFIG_HOME` is unavailable; otherwise HOME is not read |
| `poller.rs` | `resolve_stop_info()` | Same error only when it must derive a transcript path; explicit paths bypass HOME |
| `poller.rs` | `derive_transcript_path()` | Same error |
| `poller.rs` | `projects_dir_for_cwd()` | Same error |
| `session.rs` | `pretrust_cwd()` | Same error |

`get_home()` is the only direct production read of HOME. Other direct HOME
operations are test fixtures that deliberately set, unset, restore, or redirect
the environment. Those accesses are documented at their call sites and do not
provide a fallback.

## Rationale

Claude configuration, trust state, and transcripts belong to the invoking
user. A `/root` fallback can target the wrong user's data, is unwritable for a
normal user, and may not exist inside a container or chroot. Failing with a
configuration error exposes the environment problem before a guessed path is
used.

The full rationale is kept with the authoritative helper in `src/util.rs` and
is referenced by the config, poller, and session doc comments.

## Verification commands

```bash
# Expected: get_home() plus deliberate set/unset/restore operations in tests.
rg -n 'var(?:_os)?\("HOME"\)' src

# Review every HOME-related Rust comment against its call site.
rg -n '^\s*(//!|///|//).*HOME' src tests

# Exercise the cross-module contract and CLI rendering.
cargo test --test home_unset
```
