# `claude -p` Compatibility Contract

| | |
|---|---|
| **Contract version** | v1 |
| **Pinned by** | `tests/claude_p_compat_contract.rs` |
| **Implementation** | `src/cli.rs` (accepted surface), `src/main.rs` (dispatch and child-arg collection), `src/session.rs` (`build_child_argv`), `src/pty.rs` (child environment), `src/prompt.rs` (input limits), `src/error.rs` (exit codes) |
| **Provenance** | bead claudepr-10f4ffa5 (2026-09-27) |

The README calls claude-print a drop-in, wire-compatible replacement for
`claude -p`. That claim spans six observable axes — the accepted flags, the
prompt input handling, the output modes, the exit codes, the signal mapping,
and the child argv/environment — which the individual suites each pin only in
part. This note is the single normative definition of the whole surface: what
an invocation accepts, what it does with the prompt, what it writes, how it
exits, and exactly what argv and environment the inner `claude` process is
given. The test named above holds this document against the implementation in
both directions, so the README's headline claim has one place where its full
meaning is defined and kept true.

Byte-level output shapes are out of scope here — `docs/notes/output-format-contracts.md`
owns the text/json/stream-json bytes, `docs/notes/stream-json-contract.md`
owns the stream-json framing, and `docs/notes/config-file-contract.md` owns
the config file. This document defines the CLI surface around them.

Throughout, **the child** means the `claude` process claude-print spawns under
a PTY, and **claude-print-local** means a flag whose effect stays inside the
claude-print process and never reaches the child's argv.

## 1. Supported flags

The table below is the **complete** accepted option surface of an ordinary
invocation: every long flag the CLI parser defines appears exactly once, and
every flag listed here is accepted by the parser. The **Child argv** column is
the closed forwarding vocabulary the test enforces:

- `never` — claude-print-local; the flag (and its value) must not appear on
  the child argv.
- `verbatim` — forwarded as-is when the flag is given.
- `always: …` / `becomes: …` — derived forwarding; the named child-side form
  is what may appear, nothing else.
- `entry point` — dispatches to a non-session mode before prompt handling.

| Flag | Short | Default | Child argv |
|------|-------|---------|------------|
| `[PROMPT]` | — | — | never (injected into the PTY, see §2) |
| `--input-file <FILE>` | `-f` | — | never |
| `--model <MODEL>` | `-m` | `claude-sonnet-4-6` | always: `--model <resolved>` |
| `--max-turns <N>` | — | `30` | always: `--max-turns <resolved>` |
| `--output-format <FORMAT>` | `-o` | `text` | never |
| `--allowedTools <LIST>` | — | — | verbatim |
| `--disallowedTools <LIST>` | — | — | verbatim |
| `--dangerously-skip-permissions` | — | — | verbatim |
| `--timeout <SECS>` | — | `3600` | never (claude-print's own wall-clock watchdog; claude has no such option) |
| `--first-output-timeout <SECS>` | — | `90` | never |
| `--stream-json-timeout <SECS>` | — | `90` | never (arms only in stream-json mode) |
| `--stop-hook-timeout <SECS>` | — | `120` | never |
| `--claude-binary <PATH>` | — | `claude` via `PATH` | never |
| `--pool-socket <PATH>` | — | — | never |
| `--config <FILE>` | — | XDG, then `~/.config` | never |
| `--no-inherit-hooks` | — | off | becomes: `--setting-sources=` |
| `--mcp-config <MCP_CONFIG>` | — | — | becomes: `--strict-mcp-config` + one `--mcp-config <entry>` per entry |
| `--pretrust-cwd` | — | off | never (writes `~/.claude.json` before spawn) |
| `--show-child-stderr` | — | off | never |
| `--verbose` | — | — | never |
| `--check` | — | — | entry point |
| `--clean` | — | — | entry point (requires `--check`) |
| `--version` | `-V` | — | entry point |
| `--help` | `-h` | — | entry point |

`never` is not a value judgment: for `--timeout` and the other watchdog flags
it is load-bearing. Forwarding `--timeout` to the child broke every invocation
against claude 2.1.263 with `error: unknown option '--timeout'` before the
prompt was injected (`tests/flag_compat.rs` exists because of it).

`--pretrust-cwd` has a dedicated contract:
`docs/notes/pretrust-cwd-contract.md` defines the trust-file schema it writes, the
merge and idempotency semantics, the failure taxonomy, and the
trusted/untrusted-directory e2e coverage that this table's one-line summary
compresses.

`--show-child-stderr` has a dedicated contract:
`docs/notes/show-child-stderr-contract.md` defines what the capture holds, the
three exit windows that dump it, the exact stderr block, how the output modes
and the pool/fallback paths treat it, and the end-to-end coverage that this
table's one-line summary compresses.

The `serve` subcommand is claude-print's own daemon entry point — real `claude`
has nothing like it, so it is not part of the compatibility surface, but its
flags are part of the parser and listed for completeness:

| Flag | Default | Notes |
|------|---------|-------|
| `serve --pool-size <N>` | `1` | 1–256; 0 or >256 exits 2 before any spawn |
| `serve --socket <PATH>` | `/tmp/claude-print-pool.sock` | |
| `serve --verbose` | off | |

The derivation rules referenced above:

- `--model <resolved>`: CLI `--model` > config `defaults.model` >
  compiled-in `claude-sonnet-4-6`. Always forwarded — the compiled-in default
  is applied rather than left to the child's own fallback.
- `--max-turns <resolved>`: CLI `--max-turns` > config `defaults.max_turns` >
  compiled-in `30`. Always forwarded for the same reason.
- `--setting-sources=`: forwarded only in isolation mode — `--no-inherit-hooks`
  or config `inherit_hooks = false`. Default mode omits it so the user's own
  hooks fire alongside the relay hook, exactly as `claude -p` behaves.
- `--strict-mcp-config` + `--mcp-config <entry>`: forwarded only when
  `--mcp-config` names at least one config; one pair per entry, so only the
  named configs load.

## 2. Prompt input (stdin handling)

- **Sources, in precedence order**: `--input-file <FILE>`, then the positional
  `[PROMPT]`, then stdin (read only when stdin is not a TTY). At most one
  source is read: when several are supplied the higher-precedence one wins and
  the others are ignored.
- **The prompt never travels on the child argv.** The resolved bytes are
  injected into the PTY as a bracketed-paste envelope (`ESC[200~` … `ESC[201~`
  followed by CR) after the trust dialog is dismissed.
- **Size cap**: every source is bounded by `PROMPT_MAX_BYTES` = 10 MiB
  (10 × 1024 × 1024 bytes). `--input-file` is size-checked and type-checked
  (regular files only) *before* its contents are read; stdin is bounded during
  the read. Exceeding the cap is a policy rejection: exit 2,
  `claude-print: stdin is larger than the 10485760-byte limit` (the
  `--input-file` form names the file and its size).
- **NUL bytes**: rejected from every source (real `claude -p` does not support
  them) — exit 2, `claude-print: prompt contains a null byte at offset <N>
  (not supported)`.
- **Empty prompt**: an empty stdin, or no source at all, exits 4 with
  `claude-print: no prompt provided (pass as argument, --input-file, or stdin)`.
- **Unreadable `--input-file`**: a missing path, a non-regular file, or an
  unreadable file exits 4 with a `claude-print: …` line naming the path.

## 3. Output modes

`--output-format` selects `text` (default), `json`, or `stream-json`. The
modes are claude-print's own emitters over the session transcript — the child
is never given `--output-format` (or `--print`), because those flags activate
the print/SDK billing path the tool exists to avoid. Field-level and
byte-level contracts:

- `text` — the response text plus one LF on stdout.
- `json` — one line, one `result` object.
- `stream-json` — a live JSONL replay of this session's transcript records.

See `docs/notes/output-format-contracts.md` for the exact shapes, error
routing (which stream carries what in which mode), and the divergences from
real `claude -p` output; see `docs/notes/stream-json-contract.md` for the
stream-json framing. One routing rule matters at CLI granularity: stdout
carries the payload only, and everything human (warnings, `--verbose` traces,
`--show-child-stderr` dumps, text-mode errors) goes to stderr.

## 4. Exit codes

Mode-independent — the output format changes the *shape* of an error report,
never the process status. `subtype` is the value carried by the json and
stream-json error objects.

| Exit | `subtype` | When |
|------|-----------|------|
| `0` | `success` | The turn completed; the Stop hook fired and the transcript was read |
| `1` | `assistant_error` | The turn finished but the transcript reported `is_error: true` (rate limit, tool failure) |
| `2` | `internal_error` | claude-print-side failure: missing binary, PTY/hook setup, config read/parse/validation, child exited before Stop; also prompt-policy rejections (oversize input, NUL byte) and argv usage errors |
| `4` | *(none — plain `claude-print: …` stderr line in every mode)* | Input validation: no prompt found, unreadable/non-regular `--input-file`, stdin read failure |
| `124` | `timeout` | A watchdog deadline or the wall-clock `--timeout` expired |
| `130` | `interrupted` | SIGINT or SIGTERM during a session |

The exit-4 family fires before the emitter engages, so it has no JSON shape in
any mode — stderr only, prefixed `claude-print: `, stdout empty. Exit-2 policy
rejections (oversize prompt, NUL byte) share that pre-emitter plain-line shape.
Config failures are the one exit-2 exception with a JSON shape: they render the
structured error object, on **stderr** in every mode (stdout stays clean
because no session exists). Argv usage errors (unknown flag, missing value)
exit 2 with clap's own usage message.

## 5. Signal mapping

| Signal | Mapping |
|--------|---------|
| SIGINT (session) | Intercepted — the default kill is replaced by a handler. The session aborts, the child is torn down (SIGTERM, 2 s grace, then SIGKILL), and claude-print exits `130` with the `interrupted` error shape (`subtype: "interrupted"`). |
| SIGTERM (session) | Same handler, same outcome: teardown and exit `130`. |
| SIGWINCH | The library's PTY relay (`PtySpawner::relay`, used by `--check`'s PTY probe and by library callers) re-applies the controlling terminal's window size to the child PTY via `TIOCSWINSZ`; the same relay forwards SIGINT to the child as SIGINT and returns 130. Signal-forwarding behavior is pinned by `tests/sigint_forwarding_e2e.rs` and `tests/sigwinch_forwarding_e2e.rs`. |
| SIGINT/SIGTERM (serve) | The pool daemon shuts down gracefully: every worker torn down, the socket removed, exit `0` — a supervisor stop is not a failure. |

## 6. Child argv

The child is exec'd as `<claude-binary> --settings=… …` with the arguments, in
order:

1. `--settings=<tempdir>/settings.json` — **always, first**: the relay hooks
   (Stop + UserPromptSubmit) that make Stop observable through the FIFO.
2. `--setting-sources=` — only in isolation mode (§1 derivation rules).
3. `--strict-mcp-config`, then one `--mcp-config <entry>` per named config —
   only when any config is named.
4. `--model <resolved>` — always.
5. `--max-turns <resolved>` — always.
6. `--dangerously-skip-permissions` — only when given.
7. `--allowedTools <list>` — only when given.
8. `--disallowedTools <list>` — only when given.

**Never on the child argv**: `--print` and `-p`, `--output-format` (invariant
4 — they activate the print/SDK billing path); every claude-print-local flag
in §1's `never` class; and the prompt itself (§2 — injected via the PTY).

## 7. Child environment

Built in the parent before `fork()` and handed to `execvpe` (never mutated
post-fork). Two deltas from the inheriting parent's environment, everything
else passes through verbatim:

**Forced** (always present, overriding any inherited value):

| Variable | Value | Why |
|----------|-------|-----|
| `CLAUDE_CODE_ENTRYPOINT` | `cli` | The billing input: the child must classify as the interactive entry point (`cc_entrypoint=cli`) to bill the subscription, even when the parent inherited `sdk-cli`. |
| `CLAUDE_CODE_FORCE_SESSION_PERSISTENCE` | `1` | Keeps the transcript persistence gate on so the Stop-hook/transcript design has a file to read. |

**Scrubbed** (dropped even when inherited):

| Variable | Why |
|----------|-----|
| `CLAUDE_CODE_SESSION_ID` | An inherited session id would make the child write into the parent's transcript. |
| `CLAUDECODE` | Claude's own nested-session marker; inherited it flips the child into subagent-style mode. |
| `CLAUDE_CODE_CHILD_SESSION` | Gates transcript persistence (claude 2.1.263): inherited, the TUI disables transcript saving. |
| `CLAUDE_CODE_SKIP_PROMPT_HISTORY` | Session-marker family; dropped for a fresh top-level session. |
| `CLAUDE_CONFIG_DIR` | Transcript-placement invariant: the child must keep the real config dir so the transcript lands under `$HOME/.claude/projects/`, the only tree claude-print reads. claude-print never sets it, and an inherited value (agent cleanrooms export it) is dropped. |

## 8. Divergences from real `claude -p`

The drop-in claim is about the observable CLI surface above; the known,
deliberate differences:

1. **The `-p`/`--print` flag itself is rejected** — claude-print's parser has
   no such flag, so the one argument a caller must drop when switching is the
   print flag (usage error, exit 2). `claude-print "prompt"` replaces
   `claude -p "prompt"`.
2. **`cost_usd` is always `0`** and the json result has no `model` field —
   subscription-billed, nothing metered is measured
   (`docs/notes/output-format-contracts.md` §Divergences).
3. **`stream-json` output ends with the last transcript record**, not a
   synthesized `type: "result"` event; completion is signaled by process exit.
4. **Pre-emitter input failures are unstructured** — the exit-4/policy family
   prints plain stderr lines in every mode, where `claude -p` may shape them
   per mode.
5. **`--version` names claude-print**: `claude-print <v> (wrapping claude
   <v>)` rather than claude's own version line.

## 9. How the contract is pinned

`tests/claude_p_compat_contract.rs` holds this document against the
implementation in both directions, so a change to any axis must update this
file, the implementation, and (where one exists) the specialized contract
together:

1. **Flag surface (closed world)** — every long flag and short the real
   `Cli` parser defines appears in §1 (and the serve table), and every flag
   documented here parses; the documented defaults equal the parser's
   `default_value`s and the resolvers' compiled-in defaults.
2. **Exit codes** — §4's (exit, subtype) pairs equal `ClaudePrintError`'s
   `exit_code()`/`subtype()` accessors; the exit-4 and policy-rejection rows
   are pinned behaviorally (empty stdin → 4, NUL → 2, missing binary → 2,
   usage error → 2) with their documented stderr lines.
3. **Child argv** — §6's construction is replayed through the real
   `build_child_argv` and observed end-to-end: a baseline run's child argv is
   exactly the documented five-element default shape, and a full-featured run
   — classified per §1's Child-argv column — shows every `verbatim`/`always`/
   `becomes` form present and every `never` flag (plus the §8 forbidden
   flags) absent.
4. **Child environment** — §7's forced/scrubbed lists are pinned against
   `src/pty.rs`'s tables and observed through a live run that inherits every
   scrubbed marker plus a decoy entrypoint: the child's recorded environment
   carries exactly one `CLAUDE_CODE_ENTRYPOINT=cli`, the forced persistence
   variable, none of the five scrubbed names, and a control variable
   verbatim.
5. **Prompt input** — §2's precedence is observed with the prompt echoed back
   as the response: `--input-file` beats the positional beats stdin, and each
   source alone is used.
6. **Signals** — §5's session rows cite the exit code and subtype the error
   type actually returns, and the handler/teardown wiring is presence-pinned
   in `src/session.rs`; the relay row cites the suites that pin it
   behaviorally.
