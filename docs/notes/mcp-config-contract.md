# `--mcp-config` Contract (bound MCP init)

| | |
|---|---|
| **Contract version** | v1 |
| **Pinned by** | `tests/mcp_config_contract.rs` (syntax, delimiting edges, validation boundary, error modes); the forwarding-order/spelling/config-independence legs in `tests/binary_e2e.rs` and the pooled leg in `tests/pool_socket_e2e.rs` (bead claudepr-af36fc41); the argv shapes and the NUL guard in `src/session.rs` `#[cfg(test)]` |
| **Implementation** | `src/cli.rs` (the flag and its delimiter), `src/main.rs` (`LaunchOptions` wiring, the pooled inapplicability note), `src/session.rs` (`build_child_argv`'s strict-mode segment and NUL guard), `src/emitter.rs` (the per-mode error routing of §5) |
| **Provenance** | bead claudepr-d000e48b (2026-09-27); the strict-mode forwarding itself is bf-uj0's bound-MCP-init design, previously pinned by bead claudepr-af36fc41 |

The `claude -p` compatibility contract summarizes `--mcp-config` as one
child-argv transformation — "becomes: `--strict-mcp-config` + one
`--mcp-config <entry>` per entry" — and the config-file contract carries a
boundary section proving the flag has no config tier. Between them they
document the *forwarding* and the *config non-interaction* but not the flag's
own API: which spellings parse, what the delimiter does to edge values, how
repeated occurrences accumulate, who validates an entry's content, and what
each output mode prints when a value is bad. This note is the normative
definition of that API. The tests named above hold it against the
implementation in both directions — the same two-sided discipline as the
sibling flag contracts — so the behavior cannot move without this document,
the suites, and the implementation moving together.

Throughout, **the child** means the `claude` process claude-print spawns under
a PTY, and **an entry** means one element of the flag's accumulated value
list — one path or one inline-JSON value, however it was spelled on the
command line.

## 1. Purpose and scope

`--mcp-config` names the MCP server configurations a session may load. It
exists for headless reliability, not convenience: whenever at least one entry
is present, claude-print launches the child in strict MCP mode
(`--strict-mcp-config`), so **only the named configs load** —
inherited, project, and global MCP servers that can hang on connect (a
startup-wedge trigger from the bf-2u1 investigation) are ignored entirely.
That bound is the point of the feature (bf-uj0); this contract defines the
API surface around it.

- **A launch flag, not a setting.** It shapes the child's argv before spawn
  and has no other effect: no file is read, no validation performed, no state
  touched (§4). Its interaction with the config file — none, in either
  direction — is owned by `docs/notes/config-file-contract.md` §"`--mcp-config`:
  a config-shaped flag with no config tier".
- **Claude-print never sees MCP.** claude-print does not parse, merge, or
  count MCP servers; it cannot report "config loaded, 3 servers active". The
  flag is a pass-through with one derived companion flag (`--strict-mcp-config`)
  and one gate (§3).
- **Inert wherever no session is launched.** `--check` (with or without
  `--clean`), `--help`, and `--version` answer on their own paths with the
  flag accepted-but-unread; `serve` parses it (it sits on the top-level
  `Cli`) and never reads it; a pooled `--pool-socket` invocation cannot apply
  it to an already-running worker (§6).

## 2. Accepted syntax

- **One spelling, no short form.** The flag is `--mcp-config`; there is no
  short option.
- **A value is required.** `--mcp-config` with no value is a clap usage
  error: exit 2, clap's own usage message on stderr (`a value is required for
  '--mcp-config <MCP_CONFIG>'`), stdout empty. It is answered inside
  `Cli::parse()` — before the config step, before prompt handling, before any
  output-format logic — so the error is **mode-independent**: text, json, and
  stream-json invocations fail byte-identically, and no JSON error object is
  ever rendered for it.
- **Three equivalent per-occurrence spellings.** `--mcp-config <v>`,
  `--mcp-config=<v>`, and a comma-delimited list inside either
  (`--mcp-config a,b`, `--mcp-config=a,b`). All yield the same entries in the
  same order (pinned by `tests/binary_e2e.rs`,
  `mcp_config_comma_delimited_and_equals_spellings_match_repeated`).
- **The delimiter is a pure lexical split on the comma character.** clap's
  `value_delimiter = ','` splits each occurrence's value at every U+002C and
  keeps every segment, with no further interpretation. The consequences, all
  deliberate and pinned at parse level:
  - **Empty entries are legal.** `--mcp-config ""` yields one empty entry;
    `a,,b` yields three (`a`, ``, `b`); a lone `,` yields two empty entries;
    leading and trailing commas each contribute one (`a,` → `a` + `""`,
    `,a` → `""` + `a`).
  - **No trimming.** Whitespace is part of the entry: ` a , b ` yields
    `" a "` and `" b "`, a single space is a one-space entry. Quoting is the
    caller's business; claude-print never edits an entry.
  - **No deduplication, no reordering.** The same entry twice stays twice
    (§3 forwards both).
- **An entry is opaque text.** It may be a filesystem path or an inline JSON
  MCP-config object — claude-print does not distinguish the two (§4), so
  commas inside an inline JSON value (`{"mcpServers":{...}}` has none, but a
  future shape might) would split it; quote-safe spellings (paths, compact
  JSON) pass through untouched.

## 3. Entry list → child argv

Occurrences accumulate by concatenation in command-line order: the entry list
is exactly the lexical split of every `--mcp-config` occurrence's value, in
the order the occurrences appeared. Duplicates stay adjacent-but-separate;
later occurrences append, never prepend or merge.

`Session::build_child_argv` translates a non-empty list into this child-argv
segment, positioned after the relay `--settings=` (and after
`--setting-sources=` when isolation mode is on), before the forwarded
`--model`:

```text
--strict-mcp-config --mcp-config <entry> [--mcp-config <entry> …]
```

- `--strict-mcp-config` appears **exactly once**, leading the segment, only
  when the list is non-empty.
- One `--mcp-config <entry>` pair per entry, in entry order — a shape that is
  unambiguous regardless of what flags follow and tolerant of either path or
  inline-JSON values.
- **The gate is list emptiness, not entry emptiness.** A single empty entry
  is a non-empty list: strict mode is armed and an empty value is forwarded
  for the child to judge. claude-print second-guesses nothing here — an
  empty entry that would have been a typo'd `a,,b` reaches the child exactly
  as typed.
- An empty list (flag absent) forwards neither flag: the child's own default
  MCP resolution stays in force, which is the documented non-strict default.

## 4. The validation boundary

The flag's error surface is split by a bright line: **claude-print validates
argv shape only; the child validates content.** Everything after the split is
somebody else's contract, surfaced through claude-print's generic paths.

**claude-print-side checks — exactly two, both argv-level:**

1. clap's shape check: the value is required (§2), and an unknown flag
   spelling is clap's own usage error.
2. the argv NUL guard in `build_child_argv`: an entry containing an interior
   NUL fails `CString::new` and returns `Error::Internal` with the message
   `mcp-config value invalid: …` (exit 2, `internal_error`). This is
   defense-in-depth for library callers constructing `LaunchOptions`
   directly — unreachable from a shell, because an interior NUL cannot cross
   `execve`'s NUL-terminated argv. Unit-pinned in `src/session.rs`
   (`build_child_argv_rejects_null_bytes_in_mcp_config`).

**Everything else is Claude-side.** claude-print performs no existence check
(no `stat`), never reads a named file, never parses inline JSON, and checks
no MCP schema. Entries cross from claude-print's argv to the child's argv
byte-identically. The child resolves each entry — as inline JSON when it
parses as an MCP config, else as a path — and reports nonexistent paths,
malformed JSON, and schema violations as **its own startup failure**.
claude-print observes only the generic session outcomes of that failure:

- a child that rejects a value and exits before the Stop hook surfaces as
  the exit-2 `claude exited before Stop hook fired` family, shaped per mode
  (§5);
- a child that wedges (e.g. a *named* config whose server hangs on connect —
  strict mode narrows this class to exactly the configs the caller named)
  surfaces as the first-output watchdog's exit 124.

Two consequences are pinned behaviorally, not just stated:

- **No validation means no rejection.** An invocation whose entries name a
  nonexistent path, a malformed inline JSON object, and an empty string
  still spawns, forwards all three verbatim, and completes successfully
  whenever the child does not care — proving claude-print validated nothing
  (the mock child accepts anything; a real `claude` would reject and take
  the bullet above).
- **The child's complaint is the diagnosis.** What the child printed before
  dying — its own error text naming the bad config — is exactly what
  `--show-child-stderr` captures and dumps on a pre-injection death
  (`docs/notes/show-child-stderr-contract.md` §3). claude-print's error line
  never names the entry; the child's does.

## 5. Error handling by output mode

The mode shapes the *report*, never the exit code (compat §4). Three
families can involve this flag:

| Family | Exit | text | json | stream-json |
|---|---|---|---|---|
| clap usage error (missing value) | 2 | clap usage on stderr, stdout empty | identical — clap answers before any mode logic | identical |
| NUL guard (`mcp-config value invalid`; library callers only, unreachable via CLI) | 2 | `error: …` on stderr | one-line `result` object on stdout | one-line `result` object on stdout |
| child-side rejection (child exits before the Stop payload — the modeled Claude-side validation failure) | 2 | `error: claude exited before Stop hook fired` on stderr, stdout empty | one-line `result` object on **stdout** | one-line `result` object on **stdout** |

For the child-side rejection row — the one a bad value actually produces —
the structured shape is exactly:

```json
{"type":"result","subtype":"internal_error","is_error":true,
 "error_message":"claude exited before Stop hook fired","claude_version":"…"}
```

on stdout as a single line, in json mode and in stream-json mode alike. The
stream-json case renders the structured object unconditionally on this arm
(main's `Error::Internal` handling passes the after-inject routing), and no
transcript records were ever streamed — the session died before prompt
injection. `--show-child-stderr` adds its stderr dump block — the child's own
last words, the only place the failing config is named — in every mode,
between the run's diagnostics and the mode-shaped report; its ordering and
block shape are that contract's, not restated here. Stdout carries the
payload only: a JSON caller's stream parses exactly as it would for any
other startup failure, and the diagnosis sits on the process's stderr.

## 6. Non-session and pooled entry points

- **`serve`.** The flag sits on the top-level `Cli`, so
  `claude-print --mcp-config <v> serve …` parses with the flag accepted and
  the daemon path free to reject its own arguments (e.g. an invalid
  `--pool-size` fails exactly as without the flag). The daemon never reads
  the field: pool workers are launched by `pool.rs`'s `create_worker` with a
  fixed argv (`--settings=` plus `--setting-sources=` only), so no worker
  ever carries MCP flags from it, and there is no daemon-side spelling of
  this feature.
- **`--pool-socket` clients.** The acquired worker's `claude` is already
  running with the daemon's launch decisions, so the flag cannot reach any
  child argv. It is inert, never fatal: the prompt still runs on the
  daemon's launch, and with `--verbose` the client lists `--mcp-config` in
  its `not applied:` diagnostic alongside the other per-invocation launch
  flags (pinned by `tests/pool_socket_e2e.rs`,
  `pooled_invocation_keeps_mcp_config_off_the_worker_argv_and_reports_it_unapplied`).
- **Config file.** No config tier, no `[defaults] mcp_config` key
  (closed-world rejection names it), no resolve chain, and byte-identical
  forwarding with a config file present in any state — the boundary
  `docs/notes/config-file-contract.md` owns.

## 7. How the contract is pinned

`tests/mcp_config_contract.rs` holds each section against the implementation:

1. **Source pins (§1–§3, §6)** — the `Cli` field carries
   `value_delimiter = ','`; `main.rs` wires `cli.mcp_config` into
   `LaunchOptions::mcp_configs`; `build_child_argv` constructs
   `--strict-mcp-config` once plus one pair per entry behind the non-empty
   gate, with the `mcp-config value invalid` NUL-guard literal quoted
   against §4; the README row, the compat contract's §1 pointer, and the
   config-file contract's boundary section all stay linked to this note.
2. **Parse level (§2)** — `Cli::try_parse_from` over the delimiting edges:
   the three spellings, mixed occurrence append, the empty value, `a,,b`,
   the lone/leading/trailing comma, whitespace preservation, the
   single-space entry, duplicates, the absent flag, and `serve` accepting
   the top-level flag; the missing value is the documented usage error.
3. **Usage-error mode-independence (§5)** — the missing-value run in text
   and json modes exits 2 with byte-identical stderr, empty stdout, and no
   prompt/config complaint — clap answered first.
4. **Forwarding of the edges, end to end (§3)** — a recorded child argv
   (`MOCK_RECORD_ARGS`, empty segments preserved) shows the empty entry
   arming strict mode and the whitespace entries forwarding verbatim, one
   pair per entry in order.
5. **The validation boundary (§4)** — one invocation naming a nonexistent
   path, a malformed inline JSON object, and an empty entry still exits 0
   with its response emitted, the recorded argv carrying all three verbatim:
   claude-print validated nothing.
6. **Child-side rejection per mode (§5)** — the same `--mcp-config`-bearing
   invocation against a child that exits before the Stop payload
   (`MOCK_EXIT_BEFORE_STOP`): text keeps stdout empty with the
   `error: claude exited before Stop hook fired` line on stderr; json and
   stream-json each put exactly the one-line `result`/`internal_error`
   object on stdout; and `--show-child-stderr` adds the child's captured
   bytes on stderr while the stdout object stays intact.
7. **Isolation ordering (§3)** — `--no-inherit-hooks` plus `--mcp-config` on
   one recorded run places `--setting-sources=` before `--strict-mcp-config`.

The forwarding vocabulary itself (segment position, spelling equivalence,
config independence, the closed-world config rejection, the pooled leg) was
pinned by bead claudepr-af36fc41 and is referenced, not duplicated, here.
