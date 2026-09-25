# HOME Environment Variable Handling Strategy

**Decision:** Use strict HOME resolution. Do not fall back to `/root`, the
current directory, or another guessed location.

The authoritative rationale and behavior contract live on
`src/util.rs::get_home()`. Keeping the policy beside the helper makes it harder
for documentation and call sites to drift apart.

## Contract

`get_home()` reads `HOME` with `std::env::var_os` and:

- returns the path unchanged when the value names an existing, writable
  directory;
- returns `Error::Config` when the value is unset or empty;
- preserves valid non-UTF-8 Unix paths; and
- returns a path-specific `Error::Config` when the directory is missing,
  inaccessible, not a directory, or cannot create and write a temporary file.

The temporary write probe detects permission denial and read-only mounts, then
is removed before `get_home()` returns. Chroot and container launchers must
therefore provision or mount HOME before starting `claude-print`.

The actionable error is:

```text
HOME environment variable not set or empty; set HOME to the user's home directory
```

This is intentionally strict because Claude configuration and transcripts are
user-owned data. Guessing `/root` can select another user's directory, fails for
non-root processes, and may point outside a container or chroot. A clear setup
error is safer and easier to diagnose than reading or writing an invented path.

Filesystem failures follow these actionable forms:

```text
HOME path '/nonexistent' is not accessible: ...; set HOME to an existing, writable directory
HOME path '/home/service' is not writable: ...; grant write permission or set HOME to an existing, writable directory
```

## Call-site behavior

Every production `get_home()` call site, in resolution or validation:

| Module | Function | HOME behavior |
| --- | --- | --- |
| `util.rs` | `get_home()` | Sole production environment read; enforces the contract above |
| `main.rs` | `main()` | Validates `get_home()` before dispatch — a process-wide prerequisite check (also for early-exit entry points such as `--version`); derives no paths |
| `config.rs` | `Config::default_path()` | Uses `get_home()` only when `XDG_CONFIG_HOME` is unavailable |
| `poller.rs` | `resolve_stop_info()` | Passes `get_home` to `resolve_stop_info_with`, which consults it only when an absent transcript path must be derived |
| `poller.rs` | `derive_transcript_path()` | Always uses `get_home()` |
| `poller.rs` | `projects_dir_for()` | Always uses `get_home()`; `projects_dir_for_cwd()` and the pool path delegate here |
| `session.rs` | `Session::run()` | Validates `get_home()` at entry so direct library callers match the CLI preflight |
| `session.rs` | `Session::run_pooled()` | Same entry validation as `Session::run()`, before touching the worker |
| `session.rs` | `pretrust_cwd()` | Always uses `get_home()` for `~/.claude.json` |

An explicit Stop-hook `transcript_path` does not need HOME. Likewise, an
available `XDG_CONFIG_HOME` lets config path resolution proceed without HOME.
These are existing explicit paths, not lenient HOME fallbacks.

Production modules must call `get_home()` instead of reading HOME directly.
Tests may set, remove, or redirect HOME to exercise the contract, but test
fixtures must not synthesize `/root` when it is absent.

## Verification

The focused regression suite covers unset, empty, valid, nonexistent,
read-only, and chroot-like HOME values, as well as CLI error rendering:

```bash
cargo test --test home_unset
```

The call-site discipline itself is guarded standing: no direct HOME
environment read (`var("HOME")` / `var_os("HOME")`, or a `home_dir()`
bypass) outside `src/util.rs`, exactly one inside it, and the table above
pinned against the actual call sites:

```bash
cargo test --test home_env_guard
```

A new or moved `get_home()` call site fails that guard until this table
and its snapshot are updated in the same commit.

See [`docs/test-coverage-home-unset.md`](../test-coverage-home-unset.md) for the
individual cases and [`docs/research/home-handling-audit.md`](../research/home-handling-audit.md)
for the call-site audit.
