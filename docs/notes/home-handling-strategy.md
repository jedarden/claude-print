# HOME Environment Variable Handling Strategy

**Decision:** Use strict HOME resolution. Do not fall back to `/root`, the
current directory, or another guessed location.

The authoritative rationale and behavior contract live on
`src/util.rs::get_home()`. Keeping the policy beside the helper makes it harder
for documentation and call sites to drift apart.

## Contract

`get_home()` reads `HOME` with `std::env::var_os` and:

- returns the path unchanged when the value is non-empty;
- returns `Error::Config` when the value is unset or empty;
- preserves valid non-UTF-8 Unix paths; and
- does not require the path to exist at resolution time.

The actionable error is:

```text
HOME environment variable not set or empty; set HOME to the user's home directory
```

This is intentionally strict because Claude configuration and transcripts are
user-owned data. Guessing `/root` can select another user's directory, fails for
non-root processes, and may point outside a container or chroot. A clear setup
error is safer and easier to diagnose than reading or writing an invented path.

## Call-site behavior

| Module | Function | HOME behavior |
| --- | --- | --- |
| `util.rs` | `get_home()` | Sole production environment read; enforces the contract above |
| `config.rs` | `Config::default_path()` | Uses `get_home()` only when `XDG_CONFIG_HOME` is unavailable |
| `poller.rs` | `resolve_stop_info()` | Uses `get_home()` only when an absent transcript path must be derived |
| `poller.rs` | `derive_transcript_path()` | Always uses `get_home()` |
| `poller.rs` | `projects_dir_for_cwd()` | Always uses `get_home()` |
| `session.rs` | `pretrust_cwd()` | Always uses `get_home()` for `~/.claude.json` |

An explicit Stop-hook `transcript_path` does not need HOME. Likewise, an
available `XDG_CONFIG_HOME` lets config path resolution proceed without HOME.
These are existing explicit paths, not lenient HOME fallbacks.

Production modules must call `get_home()` instead of reading HOME directly.
Tests may set, remove, or redirect HOME to exercise the contract, but test
fixtures must not synthesize `/root` when it is absent.

## Verification

The focused regression suite covers unset, empty, valid, nonexistent, and
chroot-like HOME values, as well as CLI error rendering:

```bash
cargo test --test home_unset
```

See [`docs/test-coverage-home-unset.md`](../test-coverage-home-unset.md) for the
individual cases and [`docs/research/home-handling-audit.md`](../research/home-handling-audit.md)
for the call-site audit.
