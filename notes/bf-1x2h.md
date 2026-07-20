# bf-1x2h — CI WorkflowTemplate must build musl static binary (HR-1)

## Task
`claude-print-ci-workflowtemplate.yml` built a glibc/dynamically-linked binary on
`debian:bookworm` and gated CI on `cargo test` only — violating plan Hard Requirement 1
("single statically-linked binary") and the plan's all-gates (fmt/clippy/audit) policy.

## Outcome on dispatch
The substantive fix was **already committed** by a prior run as `2aa90a4`
("fix(ci): build static musl binaries + enforce all gates (HR-1)", message says
"closes bead bf-1x2h") and fully met every acceptance criterion. However that commit
was **never pushed** — local `main` and `origin/main` had diverged on a redundant,
byte-identical bead checkpoint (`a4adef9` local vs `af5e23d` remote, both
"Flush db → JSONL after br close bf-b5q"), so the push had been blocked. This
re-dispatch's job was to land the already-written fix and verify propagation.

## Work performed (this dispatch)
- **Verified the committed fix meets all acceptance criteria** against the file:
  `rustup target add x86_64-unknown-linux-musl`; both binaries built with
  `--target "${MUSL_TARGET}"`; source paths under `./target/<triple>/release/`;
  `verify_static()` ldd check that fails unless "statically linked"/"not a dynamic
  executable"; `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo audit` run before the release build; asset names unchanged
  (`claude-print-${TARGET}` / `mock_claude-${TARGET}`).
- **Resolved the push divergence** without force-push: stashed unrelated in-flight
  work from other beads, rebased `main` onto `origin/main` with `--empty=drop`
  (the redundant checkpoint was auto-skipped as already-applied; the fix replayed
  cleanly), pushed (fast-forward `af5e23d..3593c04`), then restored the stash.
  No other-bead content was lost (verified via `git diff HEAD`).
- **Confirmed declarative-config propagation** (the prior commit message claimed it
  was done): `k8s/iad-ci/argo-workflows/claude-print-ci-workflowtemplate.yml` in
  `jedarden/declarative-config` is byte-identical to this repo's source-of-truth,
  committed as `590e6f4`, and on `origin/main` — so ArgoCD will live-sync it.

## Result
Fix is on `origin/main` of both repos:
- claude-print: `3593c04` (rebased from `2aa90a4`)
- declarative-config: `590e6f4`

All acceptance criteria satisfied.
