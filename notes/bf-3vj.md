# Bead bf-3vj: install.sh for claude-print binary

## Status: Complete (pre-existing)

`install.sh` was created as part of Phase 9 (commit `50b2132`). No changes were required.

## Verification

`bash -n install.sh` → passes (POSIX-compatible `/bin/sh` shebang, `set -e`).

## What install.sh does

- Detects architecture (`x86_64` → `x86_64-unknown-linux-musl`, `aarch64` → `aarch64-unknown-linux-musl`)
- Downloads `claude-print-<TARGET>` from `https://github.com/jedarden/claude-print/releases/latest/download/`
- Backs up any existing binary to `~/.local/bin/claude-print.prev`
- Installs with `install -m 755` to `~/.local/bin/claude-print`
- Optionally installs `mock_claude` (skippable via `SKIP_MOCK_CLAUDE=1`)
- Installs `claude-print.yaml` to `~/.needle/agents/` if NEEDLE is present
- Runs `claude-print --check` to verify the binary works before exiting

## Acceptance criteria

- [x] `bash -n install.sh` passes
- [x] Downloads correct binary for platform from GitHub releases
- [x] Installs to `~/.local/bin/claude-print` with executable permissions (`-m 755`)
- [x] Runs `claude-print --check` after install
- [x] POSIX-compatible (`#!/bin/sh`, POSIX constructs only)
