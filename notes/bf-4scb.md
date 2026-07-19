# bf-4scb — Tag v0.2.0 and push to origin

## Task
Create and push the `v0.2.0` release tag to origin (Forgejo), which mirrors to
GitHub server-side. Tag must follow the plan's "Release Tag Convention" (semver
`v<MAJOR>.<MINOR>.<PATCH>`) and must never be force-pushed.

## Result
Both acceptance criteria passed.

## Verification

| Criterion | Result |
|-----------|--------|
| Tag `v0.2.0` exists locally | ✅ points to `043d726` |
| Tag is pushed to origin (Forgejo) | ✅ `refs/tags/v0.2.0 -> 043d726` |

### Local tag
```
$ git tag -l
v0.2.0

$ git rev-list -n1 v0.2.0
043d7267419c484d8e3ef54041ac81d17a63e236
```

### Push (no force-push)
```
$ git push origin v0.2.0
To https://git.ardenone.com/jedarden/claude-print.git
 * [new tag]         v0.2.0 -> v0.2.0

$ git ls-remote --tags origin v0.2.0
043d7267419c484d8e3ef54041ac81d17a63e236	refs/tags/v0.2.0
```

## Notes
- **Prerequisite met:** pre-release test suite was green per `bf-5biv`
  (222 passed, 0 failed, 1 ignored; `--check` exit 0). Cargo.toml version is
  `0.2.0`, matching the tag.
- **Divergence reconciled before tagging.** `main` had diverged from
  `origin/main` (1 ahead / 1 behind): the same `docs(bf-5biv)` commit existed on
  both sides with **different SHAs** (`ef17203` local vs `043d726` origin) — the
  only difference was a ~29s committer-date gap; the committed trees were
  byte-identical (`git diff ef17203 043d726` empty). A release tag must point at
  a commit reachable from `origin/main`, so local `main` was reset to
  `origin/main` (`git reset --mixed origin/main`) before tagging. The reset
  moved only the branch pointer (trees matched); the pre-existing uncommitted
  edits in `src/event_loop.rs`, `src/pty.rs`, `src/session.rs` were left
  untouched — they are test-only PATH-portability fixes belonging to other
  beads (per `bf-5biv`) and are **not** part of this release.
- Tag form is lightweight (`git tag v0.2.0`) per the plan's documented
  Release Tag Convention; no annotated tag, no `--force`.
- No source changes were made for this bead — this was a release-tagging task.
