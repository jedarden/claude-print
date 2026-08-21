# HOME Environment Variable Test Coverage

The focused regression suite is `tests/home_unset.rs`. It verifies the shared
strict HOME contract across config path resolution, transcript path derivation,
the live projects directory, and binary error output.

## Expected behavior

`src/util.rs::get_home()` returns:

```text
Error::Config("HOME environment variable not set or empty; set HOME to the user's home directory")
```

when HOME is unset or empty. It accepts every non-empty value without an eager
filesystem check and never falls back to `/root`.

There are two conditional cases:

- `Config::default_path()` does not read HOME when `XDG_CONFIG_HOME` is
  available.
- `resolve_stop_info()` does not read HOME when the Stop payload supplies an
  explicit transcript path (or when there is not enough information to derive
  one).

## Covered cases

| Test | Coverage |
| --- | --- |
| `unset_home_is_rejected_identically_by_config_and_poller` | Config and both poller path helpers return the same strict error |
| `empty_home_is_equivalent_to_unset_home` | Empty HOME has the same result as missing HOME |
| `valid_home_roots_every_derived_path` | All derived user paths remain under the configured HOME |
| `nonexistent_home_is_accepted_consistently_without_eager_io` | Resolution preserves a not-yet-created HOME path |
| `chroot_like_layout_never_falls_back_to_root_home` | An isolated layout cannot escape to `/root` |
| `binary_reports_actionable_home_error_in_every_output_format` | Text, JSON, and stream-JSON modes exit with setup code 2 and the actionable message |

The helper's unit tests additionally cover non-UTF-8 Unix paths.

## Run

```bash
cargo test --test home_unset
```

When adding a production path derived from HOME, call `get_home()` and extend
this suite if the new path introduces distinct behavior. Tests that manipulate
HOME directly should say why they bypass the helper; they must not invent a
fallback value.
