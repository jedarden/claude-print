# bf-3k2 — Repo hygiene: remove committed cruft (re-dispatch verify)

## Outcome
Re-dispatched for verification. The cruft removal was already substantially
complete on `main`; this pass re-checked every acceptance criterion and fixed
one residual: an accidentally-tracked `.force_local`.

## Prior commits that did the core work
- `1d07995 chore(bf-3k2): remove repo cruft` — removed `notes/bf-1ae5.md`,
  added `.gitignore` for `target/`, `.beads/beads.db.backup.*`,
  `.needle-predispatch-sha`, and `.force_local`.
- `5baa9b5 chore(hygiene): ignore repo-root '~' dir (bf-68x7)` — escaped-home-path
  hazard; added `/~` to `.gitignore`. Literal `~` dir no longer on disk.
- `bfe0bfc docs(notes): document notes/ worker-scratch dir in AGENTS.md + plan (bf-5yhm)`
  — `notes/` deliberately kept and documented in the plan Module Layout.

## Acceptance-criteria verification (all PASS)
1. **No literal `~` path tracked** — PASS. `git ls-files | grep '^~/'` → 0.
   Not on disk either; `.gitignore` rule `/~` guards against re-introduction.
2. **No `target/` files tracked** — PASS. `git ls-files | grep '^target/'` → 0.
   `target/` in `.gitignore`.
3. **No `beads.db` backups tracked** — PASS. `git ls-files | grep 'beads\.db.*backup'` → 0.
   `.beads/beads.db.backup.*` in `.gitignore`. `issues.jsonl` and live bead files untouched.
4. **Root contains only plan Module Layout files + allowlist** — PASS (after this change).
   The other items (`test-cleanup-verification.md`, `.needle-predispatch-sha`) are gone
   from the index / present-on-disk-but-untracked-and-ignored, as intended.

## Fix applied this pass
`.force_local` was tracked at repo root (committed by accident in `50f3fdd`) despite
being:
- empty (0 bytes),
- already listed under "NEEDLE worker artifacts" in `.gitignore` (intent: never tracked),
- absent from the plan Module Layout and the acceptance allowlist,
- unreferenced by any `.rs`/`.sh`/`.md`/`yml`/`toml`.

`.gitignore` only stops *new* tracking; it does not untrack an already-committed file.
Fix: `git rm --cached .force_local` — untracks it while leaving the (gitignored) file on
disk in case the NEEDLE dispatch harness uses it as a runtime marker. This aligns the
index with the `.gitignore` intent established in the original `bf-3k2` cleanup.

All removals via plain `git rm` in a normal commit; no force-push, no history rewrite.
