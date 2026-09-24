# Installer Rollback (`claude-print.prev`)

`install.sh` preserves the previously installed binary as
`~/.local/bin/claude-print.prev` on every upgrade, which makes a bad release
a one-step revert. The README's [Upgrades and rollback] section carries the
user-facing workflow; this note is the authoritative statement of the copy's
semantics, and `tests/install_sh.rs` pins each row of the table below
hermetically (fake releases whose binary bodies differ per generation, so a
content assert identifies which install a file came from).

[Upgrades and rollback]: ../../README.md#upgrades-and-rollback

## Semantics

| Property | Behavior |
|----------|----------|
| Creation | The copy is created only when `~/.local/bin/claude-print` already exists at install time. A fresh install (no existing binary) creates no `.prev`. |
| Replacement | Every upgrade overwrites the copy (`mv` onto the existing name). It always holds the *immediately previous* binary: there is no chain of older copies, and a version installed two upgrades ago is gone. |
| Mode | `mv` preserves the installed binary's mode, so the copy stays executable (755) — the rollback step needs no `chmod`. |
| Ordering | The backup runs *after* the downloaded artifact has passed `sha256sums.txt` verification and *before* the new binary is placed. A failed verification (missing manifest, missing checksum entry, digest mismatch) therefore leaves the live binary and any existing `.prev` untouched. |
| Scope | Only the main binary is backed up. `mock_claude` and `~/.needle/agents/claude-print.yaml` are overwritten in place with no rollback copy, so rolling the binary back does not revert them. |

## Rollback workflow

One command, then the self-check:

```sh
mv ~/.local/bin/claude-print.prev ~/.local/bin/claude-print
claude-print --check
```

Properties of that step:

- The `mv` *consumes* the copy: after rolling back there is no `.prev` until
  the next upgrade. A second consecutive rollback (two versions back) is
  therefore not possible — install the older release explicitly instead,
  pointing `CLAUDE_PRINT_RELEASE_URL` at that release's assets.
- The binary name is unchanged, so nothing that invokes `claude-print`
  (including the NEEDLE adapter at `~/.needle/agents/claude-print.yaml`)
  needs reconfiguration.
- If the bad release also shipped a bad `mock_claude`, that binary stays
  installed (it has no rollback copy); re-running `install.sh` against a
  good release restores it, or `SKIP_MOCK_CLAUDE=1` installs without it.

### The mid-install failure window

Between the backup `mv` and the new binary's `install`, a failure (disk
full, permissions) leaves the previous binary safe at `.prev` and nothing at
`claude-print`. The recovery is the same rollback `mv` above — that is why
the backup is taken before the new binary lands rather than after. The whole
window is pinned hermetically by
`mid_install_placement_failure_leaves_the_previous_binary_recoverable_and_reports_the_failure`
in the coverage table below.

## Hermetic coverage

`tests/install_sh.rs` pins the semantics against real `install.sh` runs over
fake `file://` releases (per-generation binary bodies):

| Test | Pins |
|------|------|
| `upgrade_backs_up_the_existing_binary_as_the_rollback_copy` | Creation: prior binary lands verbatim at `.prev`, mode 755, stdout records the backup; `mock_claude` gains no copy (scope) |
| `each_upgrade_replaces_the_rollback_copy_with_the_immediately_previous_binary` | Replacement: after two upgrades the copy holds the middle generation, and exactly one `*prev*` file exists (no chain) |
| `fresh_install_creates_no_rollback_copy` | No existing binary means no `.prev` |
| `a_failed_install_disturbs_neither_the_live_binary_nor_the_existing_rollback_copy` | Ordering: a digest mismatch aborts before the backup, leaving the live binary and an existing `.prev` byte-identical |
| `mid_install_placement_failure_leaves_the_previous_binary_recoverable_and_reports_the_failure` | The mid-install failure window: with verification passed and the backup taken, a failing final placement (an `install` shim on `PATH` matching only the main binary's destination, simulating the disk-full shape) exits nonzero with the tool's error on stderr, prints no success line (`Installed …`, `--check`, `Installation complete`), places nothing further, and leaves the live path vacant — while `.prev` holds the previous binary verbatim at mode 755, still runnable, and the documented rollback `mv` restores a working binary from it |
