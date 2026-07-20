# bf-3br9 — Submit claude-print-ci and verify GitHub release artifacts

## Task
Submit the `claude-print-ci` WorkflowTemplate for `v0.2.0`, monitor it to
completion, and verify the GitHub release at `jedarden/claude-print` exists with
both binary artifacts.

## Result
CI workflow submitted and **Succeeded** (exitCode 0). The `v0.2.0` GitHub
release exists, is published (non-draft, non-prerelease), and contains both
expected binaries. Both acceptance criteria are met **with two documented
discrepancies** between the task description's wording and the project's actual
conventions (see *Discrepancies*).

### Re-verification (this bead was re-dispatched)
The original run (`claude-print-ci-manual-4xzkj`) had been Argo-GC'd by the time
of re-dispatch, so the core action was repeated independently:
- A fresh manual run was submitted per the plan's "Submitting CI Manually"
  snippet with `tag=v0.2.0` → **`claude-print-ci-manual-vs42v`**, reached
  `Succeeded`/exitCode 0 in 76 s (130 CPU-s). With the release pre-existing,
  the template's idempotency guard skipped re-creation (no double-release).
- The release was re-confirmed live: `claude-print-x86_64-linux` downloaded,
  size 1,035,880 B matches API metadata exactly, and local SHA256
  `431e2630da1e02752f23ef792bc91871e08971e0367e02219d06479e78caf41f` **exactly
  matches** the GitHub API asset digest. `mock_claude-x86_64-linux` present
  (318,640 B). `ldd` confirms dynamic glibc linking (the *Discrepancies* #2 gap).

## Verification

| Criterion | Result |
|-----------|--------|
| Tag `v0.2.0` pushed to origin | ✅ `043d726` (prereq, from `bf-4scb`) |
| CI workflow submitted | ✅ `claude-print-ci-manual-4xzkj`, re-verified `…-vs42v` |
| Workflow reached terminal state | ✅ `Succeeded`, exitCode 0 (79s/76s, 132/130 CPU-s) |
| Release `v0.2.0` exists | ✅ published `2026-07-19T23:32:15Z`, `target_commitish=main` |
| Release contains both binaries | ✅ `claude-print-x86_64-linux` + `mock_claude-x86_64-linux` |

### Submission
Deployed WorkflowTemplate `claude-print-ci` (gen 3, created 2026-06-10) declares
**no** `inputs.parameters` and derives the version from `Cargo.toml`
(`version = "0.2.0"`). Submitted per the plan's "Submitting CI Manually" snippet
(passing `repo`/`revision`/`tag=v0.2.0`); Argo tolerates the unused params
harmlessly — the template ignores them.

```
$ kubectl --kubeconfig=... create -f -   # workflowTemplateRef: claude-print-ci
workflow.argoproj.io/claude-print-ci-manual-4xzkj created
```

### Run outcome
```
phase=Succeeded  finishedAt=2026-07-20T02:26:42Z  exitCode=0
resourcesDuration: cpu=132 memory=2644
```
The release for `v0.2.0` already existed when this run started (verified via the
GitHub API beforehand). The template's script is `set -ex` with exactly one
early `exit 0` — the idempotency guard:
```sh
if gh release view "v${VERSION}" --repo jedarden/claude-print > /dev/null 2>&1; then
  echo "Release v${VERSION} already exists — skipping"
  exit 0
fi
```
That guard runs **after** `cargo test --verbose`. Given `set -e` + exitCode 0 +
a pre-existing release (which would make the alternate exit-0 path,
`gh release create`, error instead), the only reachable exit-0 path is the
idempotency skip — i.e. the test suite passed and CI correctly declined to
re-create the release. (Pod was `podGC: OnPodCompletion`-deleted and Argo log
archiving is not enabled on this cluster, so the skip line itself could not be
captured; the deduction above is from script structure + exit code + pre-state.)

### Release artifacts (downloaded and inspected locally)
```
$ curl -fsSL -o claude-print-x86_64-linux \
    https://github.com/jedarden/claude-print/releases/download/v0.2.0/claude-print-x86_64-linux
$ ldd claude-print-x86_64-linux
	linux-vdso.so.1
	libgcc_s.so.1 => .../libgcc_s.so.1
	libc.so.6      => .../glibc-2.40-66/lib/libc.so.6
	/lib64/ld-linux-x86-64.so.2 => .../ld-linux-x86-64.so.2

Asset                         Size (B)   Matches API
claude-print-x86_64-linux     1,035,880  ✅
mock_claude-x86_64-linux        318,640  ✅
```
Both are ELF x86-64 binaries with byte-exact sizes matching the release
metadata. The release body's exact wording ("Built from commit: …", the
`install.sh` one-liner, the "Assets" list) matches the deployed template's
`gh release create --notes` verbatim, confirming the release was produced by
this CI path (in a prior run, GC'd). It was built from commit `2ca7b9c`
(`origin/main` HEAD at run time — the template clones `main` HEAD, not the tag
`043d726`; both commits carry `version = "0.2.0"`).

## Discrepancies (task wording vs. project reality)

1. **Asset naming — `x86_64-linux`, not `linux-amd64`.** The task's acceptance
   names the assets `claude-print-linux-amd64` / `mock-claude-linux-amd64`, but
   the deployed CI produces `claude-print-x86_64-linux` /
   `mock_claude-x86_64-linux` (`TARGET="${ARCH}-linux"`). **This is correct for
   the project**: `install.sh` downloads `claude-print-${TARGET}` with
   `Linux-x86_64) TARGET="x86_64-linux"`, so a `linux-amd64`-named asset would
   break `install.sh`. The `linux-amd64` naming in the task reflects the plan's
   Phase-11 ideal, not the deployed/`install.sh` convention.

2. **Linking — dynamic (glibc), not statically linked.** The task says both
   binaries are "statically linked". They are **dynamically linked** to glibc
   (`libc.so.6`, `libgcc_s.so.1`, `ld-linux-x86-64.so.2` — confirmed via `ldd`).
   The deployed CI uses `cargo build --release`, not the
   `x86_64-unknown-linux-musl` cross-build mentioned in the README/plan Phase 11.
   Producing static musl binaries is a **pre-existing Phase-11 gap in the
   `claude-print-ci` template** (lives in `jedarden/declarative-config`) and is
   out of scope for this submit-and-verify bead; tracked separately.

The core deliverable — a published `v0.2.0` release carrying both binaries,
producible by `claude-print-ci` — is achieved and verified.

## Notes
- No source changes were made for this bead — submit/monitor/verify only. This
  file is the required commit artifact.
- GitHub secret `github-webhook-secret` (key `token`) exists in `argo-workflows`;
  the public repo (`jedarden/claude-print`, `private: false`) makes the release
  verifiable via the unauthenticated API.
