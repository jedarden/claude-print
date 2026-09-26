# Release Runbook (Forgejo → GitHub Releases)

`git.ardenone.com/jedarden/claude-print` (Forgejo) is the canonical
repository; the GitHub repo is a read-only push mirror, and GitHub Releases
is the supported artifact host. Those two facts together raise the question
this note answers end to end: how does a commit on Forgejo become a
verifiable set of downloaded bytes on an installing machine, and in what
order must the moving parts fire? The README's [Repository & contributions]
section carries the user-facing summary and the [Release checklist] the
pre-tag steps; this note is the operator runbook connecting them — commit
and tag selection, mirror publication, the supported asset names,
`sha256sums.txt` generation, version verification, and artifact validation.

The publication chain is one-directional at every hop:

```
commit on Forgejo main
  → vX.Y.Z tag on Forgejo (checklist step 6, or the workflow's own push)
    → claude-print-ci Argo Workflow builds the tagged commit
      (cloned from Forgejo — never from the GitHub mirror)
      → GitHub Release assets (gh release create, GitHub-hosted only)
        → install.sh / CLAUDE_PRINT_RELEASE_URL download + verify
```

Nothing flows back: no artifact lands on Forgejo, no ref is edited on
GitHub by hand, and the mirror carries source and tags Forgejo → GitHub
only. `tests/release_runbook_docs.rs` pins this note to the
`claude-print-ci` WorkflowTemplate so the two cannot drift apart; the
platform matrix itself is pinned by `tests/platform_matrix_docs.rs` (see
[Release assets and the platform matrix](#release-assets-and-the-platform-matrix)).

[Repository & contributions]: ../../README.md#repository--contributions
[Release checklist]: ../../README.md#release-checklist

## Commit and tag selection

The releasable unit is a commit on Forgejo `main` that has passed the
pre-tag checklist (README [Release checklist]): billing conformance
(`scripts/check-billing.sh`), the mocked test suite, a credential-free
`claude-print --check` on the build about to ship, Claude-version currency
(a new `claude --version` needs a fresh transcript fixture), and the
version bump in `Cargo.toml`.

The `claude-print-ci` WorkflowTemplate takes one parameter, `tag`:

- **empty (default) — verify-only mode**: the workflow clones `main` from
  Forgejo, runs every quality gate (contract-maintenance, fmt, clippy,
  `cargo test`, `cargo audit`), and exits *without* creating a release.
  Every push runs this mode.
- **`vX.Y.Z` — release mode**: the workflow clones exactly that tag from
  Forgejo (`git clone --depth 1 --branch "$TAG" --single-branch`), runs the
  same gates, and continues into publication.

The version that gets released is read from the *cloned tree's*
`Cargo.toml` (`grep -m1 '^version'`), and every downstream step keys off
`v${VERSION}` — the tag pushed to Forgejo, the GitHub release name, the
release title. The `tag` parameter and the tree's `Cargo.toml` version must
therefore name the same release: pass `tag=vX.Y.Z` for a tree whose
`Cargo.toml` says `X.Y.Z`. Checklist step 6 (`git tag v0.x.y && git push
origin v0.x.y`, origin = Forgejo) can push the tag ahead of the workflow;
release mode then finds the tag already present, and its fallback check —
`git tag "v${VERSION}"`, or verify `refs/tags/v${VERSION}^{commit}` equals
`HEAD` — accepts a pre-pushed tag only when it points at the exact commit
being built, and fails the build on any mismatch instead of moving it.

## Mirror publication

Why order matters: the GitHub repo is a Forgejo **push mirror**, and every
mirror sync prunes refs that exist only on GitHub. GitHub demotes a
published release to draft when its tag is deleted — so a release whose tag
lives only on GitHub is one mirror sync away from being silently demoted
(the 2026-08-16 draft-revert root cause). The invariant is that **the tag
must exist on Forgejo before any GitHub release references it**; the
workflow enforces it by pushing `refs/tags/v${VERSION}` to Forgejo before
`gh release create` ever runs.

The workflow's publication sequence, in source order:

1. Push the tag to Forgejo (skip if already tagged at `HEAD`, per above).
2. Idempotency check against GitHub (`gh release view "v${VERSION}"`):
   - **published** (`isDraft: false`) → nothing to do, exit 0 — a re-run of
     an already-shipped release is a no-op, never a re-publish;
   - **draft** → publish it in place (`gh release edit --draft=false`) and
     exit 0 — a previous run that got as far as creating a draft (or a
     release demoted by the mirror-prune failure mode) is finished, not
     duplicated;
   - **absent** → fall through to the full build and publish.
3. Build, verify, and checksum the assets (next sections).
4. `gh release create "v${VERSION}" --repo jedarden/claude-print` with the
   four assets attached.

The release is created `--target main`; the tag's canonical home is
Forgejo, which the mirror carries outward, so GitHub-side tag state is
downstream of the Forgejo push in step 1 — never an independent edit.
Forgejo hosts no release assets at any point: `install.sh`'s default
download URL is GitHub Releases, and `CLAUDE_PRINT_RELEASE_URL` redirects
it to any host serving the same assets when GitHub is unreachable.

## Release assets and the platform matrix

A release carries exactly four assets:

| Asset | What it is | Produced by |
|-------|------------|-------------|
| `claude-print-x86_64-linux` | The main binary, statically linked musl (HR-1) | `cargo build --release --target x86_64-unknown-linux-musl --bin claude-print` |
| `mock_claude-x86_64-linux` | The mock-claude test-fixture binary, same static build | `cargo build --release --target … --manifest-path test-fixtures/mock-claude/Cargo.toml` |
| `last-claude-version.txt` | The Claude Code version this release was built and contract-tested against | copied from the contract-maintenance gate's `target/last-claude-version.txt` (`unknown` only when the gate could not detect a version) |
| `sha256sums.txt` | The checksum manifest of everything above | [sha256sums.txt generation](#sha256sumstxt-generation) |

The two suffixed names are derived, not hardcoded in the publisher: CI
installs exactly one toolchain (`rustup target add x86_64-unknown-linux-musl`), derives its
build target from the runner (`uname -m` → `${ARCH}-unknown-linux-musl`,
asset suffix `${ARCH}-linux`), and cargo refuses to build a target whose
std was never installed — so a successful release can only ever carry
`x86_64-linux` asset names. That is the README's [Supported platforms]
matrix, and the agreement between the WorkflowTemplate, the README table,
and `install.sh`'s arch→asset mapping is pinned by
`tests/platform_matrix_docs.rs`: widening the release in CI, renaming an
asset, or reintroducing an installer mapping for an unpublished
architecture fails the build until all three agree.

Two further build gates run before anything is uploadable: `verify_static`
(ldd must report `statically linked` / `not a dynamic executable` for both
binaries — HR-1) and the size gate (the main binary must not exceed
10 MiB).

[Supported platforms]: ../../README.md#supported-platforms

## sha256sums.txt generation

The manifest is generated inside the workflow, after the gates and before
the upload:

```sh
sha256sum "${CLAUDE_PRINT_ASSET}" "${MOCK_ASSET}" last-claude-version.txt > sha256sums.txt
```

Two properties are load-bearing:

- **Coverage**: the manifest lists every uploaded asset *except itself* —
  the two binaries and `last-claude-version.txt`, exactly the files
  `gh release create` attaches besides `sha256sums.txt`. An asset without a
  manifest entry is uninstallable by policy (below), so coverage and the
  upload list must move together.
- **Bare filenames**: entries are recorded without a `./` prefix or `*`
  marker so each manifest name equals the release asset name byte-for-byte.
  `install.sh`'s parser (`checksum_entry_for`) tolerates both decorations
  when *reading* a manifest, but the generator emits bare names so
  validation against the GitHub asset listing is direct —
  `sha256sum -c sha256sums.txt` with the four assets beside it also
  verifies out of the box.

## Version verification

Three version surfaces must agree for a release to be coherent, and each
is checkable:

1. **`Cargo.toml` ↔ tag**: the release tag is `v${VERSION}` with `VERSION`
   read from the cloned tree's `Cargo.toml`; the pre-pushed-tag fallback
   (above) fails the build if the tag points anywhere but `HEAD`.
2. **Binary ↔ tag**: `claude-print --version` prints the crate version
   (`CARGO_PKG_VERSION` — the same `Cargo.toml` value), so an installed
   binary's `--version` output must name the release it came from.
   `install.sh` runs it as the last install step.
3. **Release ↔ Claude contracts**: `last-claude-version.txt` records the
   Claude Code version the release was built and contract-tested against
   (written by the contract-maintenance gate; `unknown` only when the gate
   could not detect a version), and the release notes carry the gate's
   status text plus the built commit (`Built from commit: ${COMMIT}`), so a
   published release is auditable back to the exact Forgejo commit and the
   exact Claude evidence it shipped with.

## Artifact validation

Consumers verify artifacts against the manifest before anything is
installed or executed; `install.sh` makes that mandatory and fails closed —
a missing manifest, an asset with no checksum entry, or any digest mismatch
aborts the install with nothing placed (`tests/install_sh.rs` pins each
case adversarially). The manifest is always downloaded first precisely
because nothing else can be verified without it.

Manual validation of a published release:

```sh
BASE="https://github.com/jedarden/claude-print/releases/download/vX.Y.Z"
for a in claude-print-x86_64-linux mock_claude-x86_64-linux \
         last-claude-version.txt sha256sums.txt; do
  curl -fsSL -O "$BASE/$a"
done
sha256sum -c sha256sums.txt     # every entry must verify
chmod +x claude-print-x86_64-linux
./claude-print-x86_64-linux --version   # must print the tag's X.Y.Z
```

After an install, `claude-print --check` (credential-free: PTY, FIFO, and
billing env-input mechanics) and `claude-print --version` are the
acceptance gates — the same pair `install.sh` runs. When GitHub is
unreachable, `CLAUDE_PRINT_RELEASE_URL` points the installer at any host
serving the *same* assets (including `sha256sums.txt`); verification is
unchanged because the manifest travels with the release.

## Hermetic coverage

| Claim | Pinned by |
|-------|-----------|
| WorkflowTemplate ↔ README matrix ↔ installer mapping agreement; asset names derived from the toolchain set | `tests/platform_matrix_docs.rs` |
| Per-row installer behavior for the matrix (refusal before download, nothing placed) | `tests/install_sh_arch.rs` |
| Fail-closed verification (missing manifest / unlisted asset / digest mismatch), rollback copy semantics | `tests/install_sh.rs` |
| The default release source the override redirects: the tag-less `releases/latest/download` base over the publisher's repo slug (workflow `--repo` flags and Forgejo clone URL agree), manifest-first fetch order, and the x86_64 asset names requested from the default | `tests/install_sh_release_source.rs` |
| This runbook ↔ the WorkflowTemplate: asset names and the toolchain claim, publication order (tag→Forgejo before `gh release create`, draft/publish idempotency before the build, manifest generation before upload), manifest coverage of exactly the uploaded assets, bare-name generation, mode/version wiring; the README's pointers back to this note (the runbook link on both release-facing surfaces, and the one-directional publication claims — canonical Forgejo repo, read-only push mirror, GitHub Releases artifact host, nothing flows back, the `CLAUDE_PRINT_RELEASE_URL` override — agreed between the two docs) | `tests/release_runbook_docs.rs` |
