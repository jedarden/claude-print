# HOME Environment Variable Test Coverage

The focused regression suite is `tests/home_unset.rs`. It verifies the shared
strict HOME contract across config path resolution, transcript path derivation,
the live projects directory, direct session startup, and binary error output.

## Expected behavior

`src/util.rs::get_home()` returns:

```text
Error::Config("HOME environment variable not set or empty; set HOME to the user's home directory")
```

when HOME is unset or empty. A non-empty value is accepted only when it names
an existing, writable directory. Missing, inaccessible, non-directory, and
non-writable paths produce path-specific configuration errors and never fall
back to `/root`.

There are two conditional cases:

- `Config::default_path()` does not read HOME when `XDG_CONFIG_HOME` is
  available.
- `resolve_stop_info()` does not read HOME when the Stop payload supplies an
  explicit transcript path (or when there is not enough information to derive
  one).

## Covered cases

| Test | Coverage |
| --- | --- |
| `unset_home_is_rejected_identically_by_config_poller_and_session` | Config, both poller path helpers, and direct session startup return the same strict error |
| `empty_home_is_equivalent_to_unset_home` | Empty HOME has the same result as missing HOME |
| `valid_home_roots_every_derived_path` | All derived user paths remain under the configured HOME |
| `nonexistent_home_is_rejected_consistently_with_path_context` | Every shared resolver rejects a missing HOME with the configured path and no `/root` fallback |
| `chroot_like_layout_never_falls_back_to_root_home` | Shared path helpers remain rooted in the configured HOME inside a synthetic jail layout |
| `actual_chroot_without_root_home_matches_unset_home_error` | The compiled CLI runs after a real `chroot(2)` into a minimal root with no `/root` and returns exactly the non-chroot missing-HOME error |
| `env_u_home_version_fails_with_actionable_error` | The literal `env -u HOME claude-print --version` scenario exits 2 without printing a success version |
| `nonexistent_home_version_fails_without_root_fallback` | The literal `HOME=/nonexistent claude-print --version` scenario reports path access failure |
| `read_only_home_version_reports_write_permission_problem` | A read-only HOME reports the path and write-permission remediation without leaving a probe file |
| `binary_reports_actionable_home_error_in_every_output_format` | Text, JSON, and stream-JSON modes exit with setup code 2 and the actionable message |

The helper's unit tests additionally cover a normal writable HOME, a regular
file supplied as HOME, read-only permissions, a mocked `EROFS` read-only mount,
probe cleanup, and non-UTF-8 Unix paths.

## Run

```bash
cargo test --test home_unset
```

The real-chroot case is Linux-only. It requires `unshare`, `chroot`, and `ldd`,
and either enabled unprivileged user and mount namespaces or equivalent
permission to create them. It maps the test user to root only inside a new user
namespace, copies `claude-print` and its dynamic loader dependencies into a
temporary root, and does not create `/root`. If the host disables the required
namespace facility, the case prints a specific skip reason; all portable HOME
contract cases still run.

The chroot invocation and the ordinary child invocation must both exit 2 with
empty stdout and the exact actionable missing-HOME message on stderr. A build
that silently substitutes `/root` will either continue past validation or
report a different path error, so this comparison fails on a supported host.

When adding a production path derived from HOME, call `get_home()` and extend
this suite if the new path introduces distinct behavior. Tests that manipulate
HOME directly should say why they bypass the helper; they must not invent a
fallback value.
