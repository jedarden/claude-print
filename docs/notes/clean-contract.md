# `--clean` Contract (orphaned temp-dir cleanup)

| | |
|---|---|
| **Contract version** | v1 |
| **Pinned by** | `tests/binary_e2e.rs` §"--check --clean" (end to end), `tests/cli.rs` (parser), `src/check.rs` `#[cfg(test)]` (scan and removal semantics) |
| **Implementation** | `src/cli.rs` (the flag), `src/main.rs` (dispatch, check-mode sweep suppression), `src/check.rs` (check-mode scan and removal), `src/hook.rs` (ordinary-invocation sweep) |
| **Provenance** | bead claudepr-591cd6c1 (2026-09-27) |

Every claude-print session stages its relay artifacts — `settings.json`,
`hook.sh`, `identity.sh`, `stop.fifo`, and `session-identity.json` — in an
owner-only temp directory named `claude-print-<pid>-<rand>` under the
process temp dir (`$TMPDIR` if set, else `/tmp`). Normal exits remove it
(`HookInstaller`'s drop guard, `session::cleanup_temp_dir`, and the
serve-mode teardown all guarantee that). A crash — SIGKILL, a lost
machine, a bug — leaves the directory behind: an *orphan*. Its FIFO can
never be answered again, and its files hold a dead session's payload.
claude-print therefore sweeps orphans by name, age, and type; `--clean`
is the user-facing handle on the check-mode half of that sweep. This note
is the normative definition of what `--clean` removes, what it can never
touch, what it prints, and how it exits.

The flag itself is one line in the accepted surface —
[the `claude -p` compatibility contract](claude-p-compat-contract.md) §1
lists `--clean` as an entry point that requires `--check` — and
`docs/config-error-analysis.md` places it inside check mode. This note
owns everything those one-liners compress.

## 1. Two sweeps, one predicate

claude-print removes orphaned temp directories in two places. Both scan
the *single* process temp directory — `std::env::temp_dir()`, honoring
`$TMPDIR` — at its top level only, select entries whose name starts with
`claude-print-`, consider directories only, and age them by **mtime**.
They differ in threshold, in the extra safety gate, and in who runs them:

| | Check-mode scan (`--check`) | Ordinary-invocation sweep |
|---|---|---|
| Where | `check::process_orphans`, run as step 6 of the check sequence | `hook::cleanup_orphans`, run at the top of `main()` |
| When it runs | only for `--check` invocations | every invocation that is **not** `--check` (plain sessions, `--version`, `serve`) |
| Age threshold | mtime aged **≥ 1 hour** | mtime aged **> 60 seconds** |
| Liveness gate | **none** — the 1h threshold is the only safety boundary | embedded `<pid>` must name a **dead** process (`kill(pid, 0)` fails); `EPERM` counts as alive, and a name with no parseable PID is left alone |
| Output | stdout, as check rows/lines (§4) | stderr notes (`claude-print: cleaned up orphaned temp dir: …`) |
| Can fail the run | yes — only a failed *requested* removal (§5) | never — best-effort, failures are stderr warnings |

`--check` owns its scan: the automatic ordinary-invocation sweep is
suppressed on every `--check` invocation so that a check run's report is
the single description of what happened to the temp dir — warn-only under
plain `--check`, removal under `--check --clean`, and nothing removed
behind the report's back.

The two thresholds are not an accident. A live session's temp dir stops
gaining entries after the prompt is injected, so its *mtime* settles at
setup time almost immediately: a short threshold alone would eventually
age out a running session's directory. The ordinary sweep therefore adds
the PID-liveness gate and only claims dirs whose owner provably exited.
Check mode runs from an operator's explicit `--check` and has no PID
context it can trust (a scanned dir's PID may have been reused by an
unrelated process, which would read as "alive" and never be reclaimed),
so there the one-hour threshold is the boundary: `--check --clean`
removes matched directories **without a liveness probe**. A session
that has been live for over an hour, with no temp-dir entry changes in
that window, is within `--check --clean`'s removal scope — schedule
`--check --clean` for idle windows (cron between sessions, post-crash
cleanup), not alongside long-running turns.

## 2. What `--check --clean` removes

An entry of the process temp directory is an orphan — a removal
candidate — exactly when **all** of the following hold:

1. **Top level only.** The scan reads one directory listing
   (`read_dir` on `std::env::temp_dir()`) and never recurses. A
   `claude-print-*` directory nested inside another directory is not a
   scan candidate (it goes away with its parent if the parent is
   removed, but is never discovered on its own).
2. **Name prefix.** The entry's file name starts with
   `claude-print-`. The rest of the name is not validated — there is
   no `<pid>-<rand>` shape requirement in check mode (unlike the
   ordinary sweep, which needs the PID and skips unparseable names).
3. **Directory.** The entry is a directory (`DirEntry::metadata`,
   which does not follow symlinks — a `claude-print-*` symlink, even
   one pointing at a directory, is never a candidate and never
   followed). Files named `claude-print-*` are ignored.
4. **Aged.** `now − mtime ≥ 1 hour` (`mtime` read from the same
   metadata). Exactly-one-hour-old counts as aged. An entry whose
   mtime is in the *future* (clock skew) yields no duration and is
   skipped — never a candidate, never an error.

Removal is `remove_dir_all`: the matched directory and everything
inside it. The scan is sorted by path, so with multiple orphans the
`CLEANED:` lines come in path order and the operation is deterministic.

## 3. Safe-scope guarantees

What `--check --clean` can never touch, even when it sits in the same
directory as real orphans:

- **Anything not named `claude-print-*`** — other tools' temp dirs,
  unrelated leftovers, files of any name.
- **Anything younger than the threshold** — a fresh claude-print temp
  dir from a session that is running (or died moments ago) is never a
  candidate at the 1h threshold.
- **Files and symlinks** with the prefix — only real directories are
  removed; a `claude-print-*` file (the check's own
  `claude-print-check-<pid>.fifo` probes, for instance) is invisible
  to the scan.
- **Anything outside the one temp directory** — no recursion, no
  globbing, no traversal into a matched dir's siblings. `$TMPDIR`
  relocation moves the whole scan with it.
- **Anything on a plain `--check`.** Without `--clean`, check mode is
  warn-only: identical scan, identical report, zero removals.

## 4. Output

Check mode prints the probe table (binary, `openpty`, `mkfifo`,
optional `mock_claude` PTY round-trip, billing entrypoint) first, then a
blank line, then one line per scan finding — on **stdout**, after the
table:

| Situation | Line (stdout) |
|---|---|
| Plain `--check`, orphan found | `WARNING: found orphaned temp dir <path> (<H.H>h old) — run rm -rf to clean up` |
| `--check --clean`, removed | `CLEANED: removed orphaned temp dir <path> (<H.H>h old)` |
| `--check --clean`, removal failed | `WARNING: failed to remove orphaned temp dir <path>: <io error>` |

`<H.H>` is the age in hours to one decimal place. No orphans means no
lines in this section — silence is the no-findings signal. The final
verdict line is unchanged by the flag: `All checks passed.` on stdout
when everything passed, `One or more checks FAILED.` on stderr
otherwise.

## 5. Exit codes

| Exit | When |
|---|---|
| `0` | All probe rows pass — and, under `--check --clean`, every requested removal succeeded. Warn-only findings (plain `--check`) never affect the verdict. |
| `2` | Any probe row failed; **or** `--check --clean` failed to remove at least one matched directory (the `WARNING: failed to remove …` case — the run asked to clean and could not finish, which is a failed check, not a warning); **or** the invocation was a usage error (`--clean` without `--check`: clap rejects it at parse time, exit 2, `error: the following required arguments were not provided: --check` on stderr). |

`--clean` is not an entry point on its own — it is a modifier of
`--check`. The parser enforces `requires = "check"`, so `--clean` alone
exits 2 before any scan runs and removes nothing, and `--check --clean`
is order-independent (`--clean --check` parses identically). `--clean`
takes no value.

## 6. Idempotency and missing artifacts

- **No orphans** (empty temp dir, nothing aged, nothing matching):
  `--check --clean` exits 0, prints no orphan lines at all. `--clean`
  never requires an artifact to exist.
- **Second run**: every matched directory from the first run is gone,
  so the second run reports nothing and exits 0 — `--check --clean` is
  idempotent, and safe to schedule on a timer.
- **Failed removals re-report**: a directory that could not be removed
  (permission denied on its contents, a file swapped in where a
  subdirectory was expected) stays in place, is reported again by the
  next run, and keeps the exit at 2 until an operator resolves it.
- **Partial contents**: a matched directory is removed with whatever
  it contains — a `stop.fifo`, a `settings.json`, arbitrary user files
  planted inside an orphaned dir all go with it. That is the point
  (orphans are claude-print's own prefix-scoped debris), and it is why
  the scope guarantees in §3 are name-and-age-bound.

## 7. How the contract is pinned

- **Parser** — `tests/cli.rs`: `--check --clean` parses with both flags
  set, in either order, and alongside other flags; `--clean` alone (and
  with a positional prompt) is rejected as
  `ErrorKind::MissingRequiredArgument`.
- **Scan and removal semantics** — `src/check.rs` unit tests: the
  exactly-at-threshold boundary, future-mtime entries, files and
  symlinks, non-recursion, missing temp dir, warn-only
  non-destruction, removal scope (stale matching dir removed; fresh
  and unrelated entries preserved), idempotency, and the
  failed-removal report shape (skipped for root, where mode bits do
  not deny).
- **End to end** — `tests/binary_e2e.rs` §"--check --clean": the
  compiled binary against a controlled `TMPDIR` — stale orphan removed
  with a `CLEANED:` line and exit 0 while fresh, unrelated, and
  non-directory entries survive; plain `--check` warns and preserves;
  a blocked removal fails the check (exit 2); no-orphans silence;
  idempotent rerun; and `--clean` alone exits 2 with clap's
  `--check` requirement on stderr, having removed nothing.
