# Installer Rollback (`claude-print.prev`)

`install.sh` preserves the previously installed binary as
`~/.local/bin/claude-print.prev` on every upgrade, which makes a bad release
a one-step revert. The README's [Upgrades and rollback] section carries the
user-facing workflow; this note is the authoritative statement of the copy's
semantics and of the operator procedures around it — upgrade, failed-upgrade
triage, rollback, and post-rollback verification. `tests/install_sh.rs` pins
each row of the table below hermetically (fake releases whose binary bodies
differ per generation, so a content assert identifies which install a file
came from).

[Upgrades and rollback]: ../../README.md#upgrades-and-rollback

## Semantics

| Property | Behavior |
|----------|----------|
| Creation | The copy is created only when `~/.local/bin/claude-print` already exists at install time. A fresh install (no existing binary) creates no `.prev`. |
| Replacement | Every upgrade overwrites the copy (`mv` onto the existing name). It always holds the *immediately previous* binary: there is no chain of older copies, and a version installed two upgrades ago is gone. |
| Mode | `mv` preserves the installed binary's mode, so the copy stays executable (755) — the rollback step needs no `chmod`. |
| Ordering | The backup runs *after* the downloaded artifact has passed `sha256sums.txt` verification and *before* the new binary is placed. A failed verification (missing manifest, missing checksum entry, digest mismatch) therefore leaves the live binary and any existing `.prev` untouched. |
| Scope | Only the main binary is backed up. `mock_claude` and `~/.needle/agents/claude-print.yaml` are overwritten in place with no rollback copy, so rolling the binary back does not revert them. The adapter copy's own contract (detection, source, mode, overwrite) is [`installer-needle-adapter.md`](installer-needle-adapter.md). |

## Operator procedures

The lifecycle in procedure order. Each step names the state it leaves
behind and how to verify it; the test pinning each claim is named in
[Hermetic coverage](#hermetic-coverage).

### Upgrade

```sh
sh install.sh
```

A successful upgrade prints, in order: `Verified claude-print-x86_64-linux
(sha256 …)` (the downloaded artifact passed the manifest check), `Backing up
existing binary to ~/.local/bin/claude-print.prev`, `Installed
~/.local/bin/claude-print`, then the `--check` smoke (`Running
claude-print --check...`) and `Installation complete.` — that banner is the
success marker; no failed run prints it. Afterwards the live path holds the
new version and `.prev` the immediately previous one.

A nonzero exit is a failed upgrade: run the triage below before anything
else.

### Fresh install versus upgrade

The two differ in exactly one precondition — whether
`~/.local/bin/claude-print` already exists when the script starts — and in
one output line:

| | Fresh install | Upgrade |
|---|---|---|
| Backup line | never printed (nothing to back up) | `Backing up existing binary to …` |
| Left behind | the new binary only; **no `.prev`** | the new binary plus `.prev` (the immediately previous version) |

One command distinguishes the states on any machine:

```sh
ls -l ~/.local/bin/claude-print ~/.local/bin/claude-print.prev
```

A consequence for failures: a *fresh* install that fails mid-install leaves
no rescue copy either — there was no prior binary to preserve — so its
recovery is simply fixing the cause and re-running; the rollback procedure
below applies only where a `.prev` exists. (Pinned by
`fresh_install_creates_no_rollback_copy`.)

### Failed upgrade triage

Every failed run exits nonzero with its reason on stderr (`Error: …`) and
never prints `Installation complete.` What to do next depends on what the
failed run left at the live path — and the filesystem answers that without
the failed run's scrollback:

```sh
ls -l ~/.local/bin/claude-print ~/.local/bin/claude-print.prev
```

| Live path holds | Failure stage | State | Recovery |
|---|---|---|---|
| The previous version (unchanged) | Verification-stage abort — missing manifest, unlisted asset, digest mismatch, undownloadable asset: everything the installer checks *before* the backup | Live binary and any existing `.prev` byte-identical to before the attempt | None needed. Fix the cause (mirror, release manifest, network) and re-run `sh install.sh`. |
| Nothing; `.prev` holds the previous version | Mid-install placement failure — [the window](#the-mid-install-failure-window) below | Previous binary safe at `.prev`; the live path is vacant | The [rollback](#rollback-workflow) `mv` restores it; confirm with `--check`, fix the cause (typically disk or permissions at `~/.local/bin`), then retry the upgrade. |
| The new version; `.prev` the previous | Placement succeeded; a later leg aborted — the `mock_claude` leg or the `--check` smoke (`Error: claude-print --check failed`) | The main binary is the new release; the run stopped short of `Installation complete.` (a `mock_claude`-leg failure also skips the NEEDLE adapter leg) | The binary itself is placed. Investigate the reported failure; re-run `sh install.sh` to complete the skipped legs, or roll back deliberately if the release is not trusted. |

The middle row is the only state in which `claude-print` can be missing
after an install attempt — from a caller's side it surfaces as
`claude-print: not found`:

```sh
command -v claude-print || ls -l ~/.local/bin/claude-print.prev
```

(no live binary, a `.prev` with a fresh mtime = the mid-install window).
Row one is pinned by
`a_failed_install_disturbs_neither_the_live_binary_nor_the_existing_rollback_copy`;
row two by
`mid_install_placement_failure_leaves_the_previous_binary_recoverable_and_reports_the_failure`
— which also pins that no success line appears: no `Installed …`, no
`--check` smoke, no completion banner, and nothing placed after the failed
step.

### The mid-install failure window

Between the backup `mv` and the new binary's `install`, a failure (disk
full, permissions) leaves the previous binary safe at `.prev` and nothing at
`claude-print`. The recovery is the same rollback `mv` below — that is why
the backup is taken before the new binary lands rather than after. The whole
window is pinned hermetically by
`mid_install_placement_failure_leaves_the_previous_binary_recoverable_and_reports_the_failure`
in the coverage table below; detection of the state is the middle row of
the [triage table](#failed-upgrade-triage).

### Rollback workflow

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

### Post-rollback verification

The `--check` in the snippet is the acceptance gate, and `--version`
confirms the generation:

```sh
claude-print --check
claude-print --version
```

- `--check` is credential-free and runs no session (README [Self-check]):
  exit 0 means the restored binary's PTY, FIFO, and billing-env mechanics
  are intact.
- `--version` must name the version the failed or bad upgrade replaced —
  the one that was live before the attempt.
- A `--check` failure *after* a rollback means the previous version is
  itself broken on this machine: install a known-good release explicitly,
  pointing `CLAUDE_PRINT_RELEASE_URL` at that release's assets — the same
  escape hatch as the two-versions-back case above.

[Self-check]: ../../README.md#self-check

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
