# `--pretrust-cwd` Trust Pre-Grant Contract

| | |
|---|---|
| **Contract version** | v1 |
| **Pinned by** | `tests/pretrust_cwd_contract.rs` |
| **Implementation** | `src/cli.rs` (the flag), `src/main.rs` (`LaunchOptions` wiring and the pooled inapplicability diagnostic), `src/session.rs` (`pretrust_cwd` / `pretrust_cwd_at` and the `run_inner` gate), `src/util.rs` (`get_home`) |
| **Provenance** | bead claudepr-bd36930d (2026-09-27) |

The `claude -p` compatibility contract
(`docs/notes/claude-p-compat-contract.md` §1) summarizes `--pretrust-cwd` as
"`never` (writes `~/.claude.json` before spawn)". This note is the normative
definition behind that one line: the exact file and schema written, when the
write happens and on which entry points, what is preserved when the file
already exists, what happens on every failure shape, and the proof that the
flag never reaches the child. `tests/pretrust_cwd_contract.rs` holds this
document against the implementation in both directions — the same
two-sided discipline as the compat contract — so the flag's behavior cannot
move without the document, the suite, and the implementation moving together.

Throughout, **the child** means the `claude` process claude-print spawns under
a PTY, **the trust file** means `$HOME/.claude.json`, and **the scanner** means
claude-print's PTY keyword scanner (`src/startup.rs`), which recognizes and
dismisses the trust dialog from the child's rendered output.

## 1. Purpose and scope

Claude Code reads folder trust **only from the trust file**
(`projects[<cwd>].hasTrustDialogAccepted`), never from `--settings`. In an
untrusted working directory the child therefore renders the one-time trust
dialog, and claude-print's only in-session line of defense is the scanner. The
scanner can miss a dialog whose trusting entry it cannot positively identify —
in that case the run fails fast (exit 2) with a diagnostic naming this flag as
the escape hatch. `--pretrust-cwd` removes the dialog at the source: it writes
the trust grant into the trust file **before** the child is spawned, so the
child never considers the directory untrusted.

- **Default off.** The trust file is shared user config, typically also
  written by interactive claude sessions; claude-print does not mutate it
  unless asked. Enable the flag where trust-dialog stalls are a real risk
  (fresh worktrees, fleet workers, automation).
- **Stateless prompt sessions only.** The write happens inside
  `Session::run`'s launch path — the ordinary `claude-print [flags] PROMPT`
  invocation. The other entry points never run it: `--help`, `--version`,
  `--check` (with or without `--clean`), and `serve` exit before any session
  exists.
- **Pooled invocations: reported, never applied.** A pooled worker's `claude`
  was launched by the daemon before the client existed, so no per-invocation
  child-launch flag can reach it. On the `--pool-socket` path the flag is
  inert: nothing is written (the pretrust function is never called — the
  pooled launch path has no pretrust step), and with `--verbose` the client's
  `not applied:` diagnostic names `--pretrust-cwd` alongside the other inert
  flags. The mechanism is shared with `--mcp-config` and is pinned
  behaviorally for that flag by `tests/pool_socket_e2e.rs`.

## 2. Timing

Inside the stateless launch path the write is bracketed by version resolution
and child construction, in this order:

1. Resolve the claude version (`<claude-binary> --version` — a plain probe
   subprocess, not the session child).
2. **The trust write** (`--pretrust-cwd` only).
3. Build the child argv.
4. Spawn the child under the PTY.

The ordering is load-bearing in both directions: the grant must exist before
the child reads trust (step 4), and any failure of the write must prevent the
spawn entirely (a hard failure below exits before step 3 ever produces a
child). `HOME` has already been validated by the strict resolver before
dispatch (`src/util.rs::get_home`, called for every entry point in
`src/main.rs`), so the write step always finds a usable home;
`tests/home_unset.rs` pins that contract end-to-end.

## 3. The file and the schema

**Path.** `$HOME/.claude.json`, where `$HOME` is resolved by
[`get_home`](../../src/util.rs)'s strict policy. The path is never guessed and
never relocated: `CLAUDE_CONFIG_DIR` plays no part (claude-print never sets it
and scrubs any inherited value from the child's environment — the child reads
trust from the same file the parent wrote).

**Key.** The current working directory as returned by `getcwd(3)` — absolute
and symlink-resolved — stringified lossily as UTF-8. This is the same view of
the cwd the child inherits, which is what makes the grant visible to it.

**Shape.** The grant is exactly:

```json
{
  "projects": {
    "<cwd>": {
      "hasTrustDialogAccepted": true
    }
  }
}
```

The cwd key's entry **merges** into the trust file's existing state; the
boolean is written as the JSON literal `true` and nothing else about the entry
is added or removed. A fresh file created by the pretrust write contains
exactly the object above — no scaffolding, no other keys — and is created with
mode `0600` (the trust file holds auth/session state; claude never made it
world-readable and neither does claude-print).

## 4. Merge semantics and idempotency

The write is a read-modify-write of the whole file:

1. **Read** the existing file and its mode. Absence reads as fresh (an empty
   root object, no mode to preserve).
2. **Merge** in place: `projects` is created if absent, the cwd's entry is
   created if absent, and `hasTrustDialogAccepted: true` is set on that entry.
   Every other byte of *state* survives: sibling project entries, unrelated
   root-level keys (claude's `oauthAccount`, `tipsHistory`, …), and unrelated
   fields already inside the cwd's own entry.
3. **Write atomically**: the merged document is written to a sibling temporary
   file `.claude.json.tmp-claude-print-<pid>` in the same directory (same
   filesystem, so the rename is atomic), chmod'd to the preserved or default
   mode, then renamed over the trust file. A chmod or rename failure removes
   the temporary file and fails the run (see §5) with the original file
   untouched.

**Reserialization, honestly stated.** The merged document is re-serialized
through `serde_json`: one compact line, keys in lexicographic order. A file
that was pretty-printed, key-ordered differently, or whitespace-padded is
**value-preserving but not byte-preserving** — after a pretrust run it is one
canonical single line with the same JSON values. (Exception: the soft-failure
paths in §5 leave the file byte-identical precisely because nothing is
written.) Number and string values round-trip through serde_json's value
model.

**Idempotency.** The merge is a boolean set to a constant: running the flag
twice over the same state produces a **byte-identical** file the second time,
and an already-trusted directory is re-granted without disturbance (same mode,
same values, same everything else).

## 5. Failure handling

Every outcome for a `--pretrust-cwd` run, as a closed table. "Proceeds" means
the session launches normally; the scanner remains the trust mechanism of
record whenever the write did not happen.

| Precondition | Outcome | Exit |
|---|---|---|
| Trust file absent | Created fresh: the §3 shape, mode `0600`, then the child spawns into a trusted cwd | 0 |
| Trust file present, parseable, object root | §4 merge, mode preserved, child spawns | 0 |
| Trust file unparseable (not valid JSON) | stderr warning `claude-print: warning: ~/.claude.json is unreadable (<parse error>); leaving it untouched (trust scanner remains active)`, file byte-identical, run proceeds | 0 |
| Trust file valid JSON, root not an object (array, string, …) | stderr warning `claude-print: warning: ~/.claude.json is not a JSON object; leaving it untouched (trust scanner remains active)`, file byte-identical, run proceeds | 0 |
| Trust file unreadable for any other reason (permission, a directory at the path, I/O error) | The same warning shape as the unparseable row, file byte-identical, run proceeds. Only genuine absence reads as fresh — a read error is never papered over with a fresh-file rename, which directory write permission alone would let through | 0 |
| Parseable, but `projects` is not an object | Hard error, no write, no child spawned: text mode prints `error: … projects is not an object` on stderr; json/stream-json emit the structured error object (`subtype: "internal_error"`) | 2 |
| Parseable, but the cwd's own `projects` entry is not an object | Hard error, no write, no child spawned, `… project entry is not an object` | 2 |
| Write step fails (chmod or rename) | Temporary file removed, hard error, existing file untouched | 2 |
| `HOME` unset, empty, or not a writable directory | The standard strict-resolver config error, before any pretrust work (`tests/home_unset.rs`) | 2 |
| cwd unreadable (`getcwd` failure) | Hard error (`pretrust cwd: …`), no write, no child spawned | 2 |

The soft/hard split is deliberate. Content that cannot be parsed *at all*
could be anything — a truncated write by another process, a file a human is
mid-edit on — and the dialog is a survivable fallback, so claude-print warns
and proceeds. A *parseable* file with a conflicting structure is a definite,
actionable state: silently proceeding would launch the child untrusted and
reintroduce the very stall the flag exists to prevent, so the run fails loudly
instead.

## 6. The child's side: trusted vs untrusted directories

The flag is **claude-print-local**: it never appears on the child argv (the
compat contract's `never` class, observed through `MOCK_RECORD_ARGS`), and the
child's environment is unaffected. The grant reaches the child exclusively
through the file it reads at startup:

- **Trusted directory** (the grant present for the cwd, however it got there):
  the child renders no trust dialog and proceeds straight to the prompt. The
  session completes normally.
- **Untrusted directory** without the flag: the child renders the one-time
  trust dialog. The scanner dismisses it when it can positively identify the
  trusting entry; when it cannot, the run exits 2 with a diagnostic naming the
  trust dialog and `--pretrust-cwd`.

The e2e coverage observes both sides through the
`MOCK_TRUST_FROM_CLAUDE_JSON` seam in `test-fixtures/mock-claude`: with the
knob set, the mock reads the same trust file real claude does — a real JSON
parse, the exact §3 schema — and suppresses the dialog when the cwd is marked
trusted. The knob is paired with the mock's *unresolvable* trust wording
(`MOCK_TRUST_WORDING=unresolvable`), whose dialog forces the driver's exit-2
refusal whenever it renders. That makes the A/B non-vacuous in both
directions: no flag → the dialog renders → exit 2; `--pretrust-cwd` → the
dialog cannot render (the run succeeds) → the only mechanism that suppressed
it is the trust file the flag wrote, in the exact schema, under the exact key.

## 7. How the contract is pinned

`tests/pretrust_cwd_contract.rs` holds each section against the
implementation:

1. **Trigger, timing, and pool exclusion (§1, §2)** — source pins: the flag is
   a real `Cli` field; `Session::run`'s launch path contains the pretrust gate
   ordered after version resolution and before child-argv construction; the
   pooled launch region contains no pretrust call at all; `main.rs`'s pooled
   inapplicability list names `--pretrust-cwd`.
2. **Schema and write mechanics (§3, §4)** — observed end-to-end through the
   compiled binary: a fresh `HOME` gains the exact §3 shape at mode `0600`; a
   populated trust file keeps every sibling project, unrelated root key, and
   unrelated entry field while gaining the grant; a second run over the merged
   file is byte-identical; a pre-existing mode survives the merge.
3. **Failure handling (§5)** — each soft row observed end-to-end (warning on
   stderr, file byte-identical, no temporary file left behind, exit 0 through
   the scanner-dismissed dialog) and each hard row observed in both renderings
   (text `error: …` on stderr; json `internal_error` on stdout) with the file
   untouched and no child ever spawned.
4. **Child side (§6)** — the recorded child argv never contains the flag while
   the same run demonstrably wrote the grant; the trusted/untrusted A/B above.
