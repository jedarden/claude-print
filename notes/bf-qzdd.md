# bf-qzdd — Hygiene sweep (2026-07-21)

**Result: clean — no hygiene fixes required.** All actionable categories were already
at zero. The only checker findings are the two report-only categories that this bead
explicitly forbids acting on.

## Checker run

Script: `~/jeds-curated-skills/repo-hygiene/scripts/repo_hygiene.sh`
Repo: `/home/coding/claude-print` (Rust project, latest tag `v0.2.0`)

```
[low] dirty-working-tree — 133 finding(s)   (mostly .beads/ trace churn — REPORT ONLY)
[low] stash-pileup        — 2 finding(s)    (REPORT ONLY)
```

## Acceptance criteria — all met at 0

| Category (fix scope) | Count | Evidence |
|---|---|---|
| a) `.gitignore` gaps | 0 | `.gitignore` already covers `target/`, `.beads/beads.db.backup.*`, `.needle-predispatch-sha`, `.force_local` |
| b) tracked build artifacts | 0 | `git ls-files` matches zero `target/ node_modules/ dist/ build/ __pycache__/ *.pyc .DS_Store` (`target/` exists on disk but is correctly untracked) |
| c) dead CI workflows | 0 | no `.github/` directory at all; 0 tracked `.github/workflows/*.yml\|yaml` |
| d) README badge drift | 0 | no shields.io / GitHub-Actions badges in README.md; no version badge to drift |

No commits were made for categories a–d because there was nothing to fix.

## Not in scope (report-only, untouched as instructed)

- `dirty-working-tree` (133): `.beads/issues.jsonl` + `.beads/traces/**` churn from
  other beads' execution traces. These are bead-forge state, not hygiene debt.
- `stash-pileup` (2): `bf-6d4` and `bf5xw` sibling-bead WIP stashes. Left in place.
- Large blobs under `.beads/traces/` (e.g. 15 MB `bf-5vm/stdout.txt`, 13 MB
  `bf-6d4/stdout.txt`): bead-forge execution traces, not build artifacts and outside
  the four fix categories — not acted on.

## Notes

- No pre-commit hooks configured (no `core.hooksPath`, no husky, no active
  `.git/hooks`), so nothing could block the hygiene commits had any been needed.
- This commit (the notes file itself) is the single required commit for the bead,
  staged by explicit path — unrelated dirty-`.beads/`-tree changes were not swept in.
