# Installer NEEDLE adapter (`~/.needle/agents/claude-print.yaml`)

`install.sh` registers claude-print with NEEDLE by copying the repo's
`claude-print.yaml` adapter template into `~/.needle/agents/` when it detects
a NEEDLE installation. The README's [NEEDLE integration] section carries the
user-facing summary; this note is the authoritative statement of that leg's
semantics, and `tests/install_sh.rs` pins each row of the table below
hermetically — with the child `PATH` fully controlled for these tests, so
`command -v needle` never depends on whether the machine running the suite
has NEEDLE installed (the fleet's coding boxes do).

[NEEDLE integration]: ../../README.md#needle-integration

## Semantics

| Property | Behavior |
|----------|----------|
| Detection | The leg runs when EITHER `needle` is found on `PATH` (`command -v needle`) OR `~/.needle/agents` already exists as a directory. The second arm covers a NEEDLE install whose bin dir is off this shell's `PATH` (the agents dir is NEEDLE's own convention, so its presence is evidence enough). |
| Source | `claude-print.yaml` from the directory containing `install.sh` — the checkout, NOT a release artifact: the file is never downloaded, has no `sha256sums.txt` entry, and passes no checksum verification. It is the one installed file that skips verification, because it is copied from local checkout state rather than fetched from a host that could serve anything else. |
| Destination | `~/.needle/agents/claude-print.yaml` (`$HOME`-rooted, absolute). The agents dir is created with `mkdir -p` when missing — including when the source is absent, because the `mkdir` precedes the source check. An existing agents dir is preserved as-is (its other files survive). |
| Permissions | `install -m 644`: mode 0644 always — regardless of the checkout copy's own mode (0664 in the repo) and of a drifted mode on an existing destination (a hand-chmod'd copy is forced back). |
| Overwrite | An existing adapter is replaced in place, byte-for-byte with the checkout's template. There is no backup copy (contrast the main binary's `claude-print.prev` in [installer-rollback.md]); re-running `install.sh` from a good checkout — or `git checkout claude-print.yaml` in one — is the recovery. |
| No-NEEDLE case | When neither detection arm holds, the leg is skipped in silence: nothing NEEDLE-related is printed and `~/.needle` is not created — the `mkdir` lives inside the detection branch, so an install on a NEEDLE-less machine leaves no trace of the leg. |
| Missing source | A checkout-less invocation — the `curl install.sh | sh` shape, where no `claude-print.yaml` sits beside the script — prints `Note: claude-print.yaml not found alongside install.sh — skipping NEEDLE config` and continues; the install still succeeds (the adapter is registration, not a runtime dependency). |
| Ordering | The leg runs after the binary and `mock_claude` legs and before the `--check` smoke, so any earlier failure (missing manifest, unlisted asset, digest mismatch, placement failure) places no adapter and creates no `~/.needle`. A failure inside the leg itself fails the whole install (`set -e`) before the smoke runs. |

Idempotence: re-running `install.sh` re-copies the template, so hand edits to
the installed adapter are reverted to the shipped template on the next
install run. The one workflow that matters for is the pool opt-in — README
"Warm PTY pool" step 1 says to add `--pool-socket` to the `invoke` template
in `~/.needle/agents/claude-print.yaml`; that edit lives until the next
`install.sh` run, and needs re-applying after an upgrade.

## Why the source is the checkout, not the release

The adapter template is configuration that travels with the code
(`claude-print.yaml` at the repo root — the file README's "NEEDLE
integration" section points at), updated in commits, not a build product.
Releasing it would add a fourth asset to `sha256sums.txt` and a
download-plus-verify leg for a file the operator already has beside
`install.sh` in the sanctioned invocation (`sh install.sh` from a clone —
README "Install"). The cost of that choice is the piped-invocation gap in the
Missing-source row above: `curl … | sh` has no template beside the script
and the leg degrades to the documented skip note. To register the adapter
there, copy `claude-print.yaml` from the repo (or re-run `install.sh` from a
clone).

## Hermetic coverage

`tests/install_sh.rs` pins the semantics against real `install.sh` runs over
fake `file://` releases. For this leg the child `PATH` is pinned to the
test's stub bin dir plus exactly the host tool directories `install.sh`
calls into (resolved at test time — NixOS hosts keep coreutils in
`/run/current-system/sw/bin`, not `/usr/bin`), so `command -v needle` sees
only the stub a test plants. The pinned-`PATH` builder panics if any
included directory carries a real `needle` binary, so the no-NEEDLE cases
can never pass vacuously on a NEEDLE-equipped host.

| Test | Pins |
|------|------|
| `needle_on_the_path_installs_the_repo_adapter_template_into_the_agents_dir` | Detection (command arm) against a fresh `HOME`: the checkout's template lands verbatim at `~/.needle/agents/claude-print.yaml`, mode 0644, stdout records the placement, the agents dir is created |
| `an_existing_agents_dir_alone_triggers_the_adapter_leg` | Detection (dir arm) with no `needle` reachable: same placement, and a pre-existing agents dir keeps its other files |
| `without_needle_the_agents_dir_is_not_created_and_nothing_needle_related_is_printed` | No-NEEDLE case: no `~/.needle` at all, no NEEDLE line on stdout, the binary/fixture legs unaffected |
| `an_existing_adapter_is_overwritten_in_place_at_0644_with_no_backup_copy` | Overwrite + permissions: a stale mode-0600 adapter is replaced byte-for-byte at mode 0644, and the agents dir holds exactly the adapter (no `.prev`) |
| `no_adapter_beside_the_script_skips_the_needle_leg_with_a_note` | Missing source (a staging copy of the script with no template beside it — the `curl … \| sh` shape): the documented note prints verbatim, no adapter is placed, the agents dir is still created (the `mkdir` precedes the source check), the install succeeds |
| `a_failed_install_places_no_needle_adapter` | Ordering: a tampered binary aborts before the leg — no adapter, no `~/.needle`, no adapter line on stdout |
