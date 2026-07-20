# bf-5mkv — install.sh end-to-end download test (Phase 11 deferred item)

## Task
Verify `install.sh` works end-to-end: download the v0.2.0 release assets from
GitHub Releases on a clean prefix, run `claude-print --check` (exit 0), and
confirm `mock_claude` is downloaded and functional. This satisfies the Phase 11
deferred checklist item (`docs/plan/plan.md:1039` — "install.sh end-to-end
download test (deferred from Phase 9) passes").

## Approach
`install.sh` hardcodes `INSTALL_DIR="${HOME}/.local/bin"`, so to exercise a
**clean prefix** without disturbing the real `~/.local/bin`, the test ran with
an isolated `HOME=/tmp/claude-print-test`. The real `claude` (and `needle`)
remain reachable via the inherited PATH, and the test bin dir was prepended to
PATH so `--check` could find `mock_claude` via `find_in_path` after install.

```
rm -rf /tmp/claude-print-test && mkdir -p /tmp/claude-print-test
export HOME=/tmp/claude-print-test
export PATH="/tmp/claude-print-test/.local/bin:$PATH"
sh ./install.sh
```

## Result — PASS
`install.sh` exited 0 and produced exactly the documented flow
(plan.md:804-812):

```
Downloading claude-print-x86_64-linux...
Installed /tmp/claude-print-test/.local/bin/claude-print
Installed /tmp/claude-print-test/.local/bin/mock_claude
Installed /tmp/claude-print-test/.needle/agents/claude-print.yaml   # needle on PATH → config step exercised
Running claude-print --check...
  openpty           PASS
  mkfifo            PASS
  mock_claude PTY   PASS  (PTY round-trip OK — isatty=true in child)
All checks passed.
claude-print 0.2.0 (wrapping claude 2.1.203 (Claude Code))
```

### Download provenance (byte-verified)
Both artifacts were re-fetched directly from the GitHub release and SHA256-matched
against the installed files — proving they came from the release, not a stale
local copy:

| Artifact | Size | SHA256 | Match |
|----------|------|--------|-------|
| `claude-print-x86_64-linux` | 1,035,880 B | `431e2630da1e02752f23ef792bc91871e08971e0367e02219d06479e78caf41f` | ✅ |
| `mock_claude-x86_64-linux` | 318,640 B | `5a2b1ef0fd174dc885de4622903ea1b7cba228f74686f16077fd95d118fda9b1` | ✅ |

The claude-print digest and size match what release bead [[bf-3br9]] recorded
independently for the v0.2.0 asset — cross-validated.

### Acceptance

| Criterion | Result |
|-----------|--------|
| install.sh downloads both artifacts from GitHub Releases | ✅ via `releases/latest/download/` → v0.2.0 |
| `claude-print --check` exits 0 | ✅ CHECK_EXIT=0, all 3 probes PASS |
| `mock_claude` downloaded and functional | ✅ `--version` → `mock-claude-version-1.0.0` (exit 0); no-arg exit 0; also exercised by the passing PTY round-trip |

## Notes
- Prereq confirmed: GitHub release **v0.2.0** is `latest` and contains both
  assets (`claude-print-x86_64-linux`, `mock_claude-x86_64-linux`).
- The task description's asset name `claude-print-linux-amd64` does not match the
  project's actual naming (`claude-print-x86_64-linux`); the real asset downloaded
  correctly. (Same naming-convention discrepancy noted in [[bf-3br9]].)
- The NEEDLE config step fired because `needle` is on PATH in this environment;
  it installed `claude-print.yaml` into the isolated test HOME (no write to the
  real `~/.needle/agents/`).
- Test prefix `/tmp/claude-print-test` was left in place as evidence; it can be
  `rm -rf`'d freely (fully regenerable via `sh install.sh`).

## Re-verification (2026-07-19)
Re-dispatched bead re-ran the full flow from scratch on a fresh clean prefix
(`rm -rf /tmp/claude-print-test` then `sh ./install.sh` under `HOME=/tmp/claude-print-test`).
Identical result — install.sh exit 0, all 3 probes PASS, version `claude-print 0.2.0
(wrapping claude 2.1.203)`. Both installed artifacts re-SHA256'd against the v0.2.0
release URLs and matched byte-for-byte (same digests as the table above). `mock_claude
--version` → `mock-claude-version-1.0.0` (exit 0); no-arg exit 0. Result remains PASS;
release `latest` still resolves to v0.2.0 with both assets.
