# HOME Environment Variable Handling Audit

**Audited:** 2026-08-21
**Scope:** Production HOME reads and path-resolution call sites

## Finding

The implementation consistently uses the strict resolver in
`src/util.rs::get_home()`. It returns `Error::Config` when HOME is unset or
empty and never substitutes `/root` or another guessed directory.

The helper uses `std::env::var_os`, so a non-empty non-UTF-8 Unix path is
preserved. It does not canonicalize the path, but it requires the path to name
an accessible directory and verifies writability with a short-lived temporary
file. This reports missing mounts, permission denial, and read-only filesystems
before session startup.

## Call-site audit

| Module | Function | Result when HOME is invalid |
| --- | --- | --- |
| `util.rs` | `get_home()` | `Error::Config` with an actionable unset, access, type, or write message |
| `config.rs` | `Config::default_path()` | Same error if `XDG_CONFIG_HOME` is unavailable; otherwise HOME is not read |
| `poller.rs` | `resolve_stop_info()` | Same error only when it must derive a transcript path; explicit paths bypass HOME |
| `poller.rs` | `derive_transcript_path()` | Same error |
| `poller.rs` | `projects_dir_for_cwd()` | Same error |
| `session.rs` | `pretrust_cwd()` | Same error |

`get_home()` is the only direct production read of HOME. Other direct HOME
operations are test fixtures that deliberately set, unset, restore, or redirect
the environment. Those accesses are documented at their call sites and do not
provide a fallback.

The final repository-wide audit also checked shell utilities, documentation,
and the `mock-claude` fixture. Shell installers and diagnostic scripts expand
their own process HOME but do not resolve paths for the `claude-print` runtime.
`mock-claude` reads HOME only after production startup has validated it, and its
fixture comment explicitly rejects a `/root` fallback. No additional Rust
runtime HOME resolver exists.

In particular, `config.rs` and `poller.rs` both import
`crate::util::get_home`: `Config::default_path()` calls it for the non-XDG
branch, while `derive_transcript_path()` and `projects_dir_for_cwd()` call it
for transcript discovery. `resolve_stop_info()` delegates derived paths to
`derive_transcript_path()`. Their comments link back to the canonical strict
policy and promise the same configuration errors.

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

# Inspect every repository HOME read or expansion and every /root mention.
rg -n 'HOME|/root' --glob '!target/**' --glob '!.git/**' .

# Expected production path consumers: config, poller, and session all import
# and call the shared helper; no module defines another HOME resolver.
rg -n 'get_home' src

# Review every HOME-related Rust comment against its call site.
rg -n '^\s*(//!|///|//).*HOME' src tests

# Exercise the cross-module contract and CLI rendering.
cargo test --test home_unset
```
