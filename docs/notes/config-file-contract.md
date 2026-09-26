# Configuration-File Contract

| | |
|---|---|
| **Contract version** | v1 |
| **Pinned by** | `tests/config_contract.rs` against `tests/fixtures/config_contract_examples_v1.json`; the README's Configuration-section summary (key table, shipped defaults, path precedence) by the same test (bead claudepr-746dd1c4), and the engineering analysis `docs/config-error-analysis.md`'s contract-bearing excerpts (quoted examples, path precedence and XDG edges, resolution limitation) likewise (bead claudepr-09637c58); the Claude-state/`CLAUDE_CONFIG_DIR` section and its README counterpart by `tests/claude_config_dir_docs_contract.rs` (bead claudepr-e46458c4); the `--mcp-config` boundary section by the mock-child argv recordings in `tests/binary_e2e.rs` and `tests/pool_socket_e2e.rs` (bead claudepr-af36fc41) |
| **Implementation** | `src/config.rs` (`Config::default_path`, `Config::load_or_default`, the `resolve_*` tiering, `Defaults::validate`), path selection wired by `src/main.rs`, `HOME` policy by `src/util.rs::get_home`, user-facing message shaping by `src/error.rs` (`From<Error> for ClaudePrintError`) and `src/emitter.rs` (`emit_error`) |
| **Provenance** | bead claudepr-227efdb1 (2026-09-25) |

This note is the normative definition of `claude-print`'s optional TOML
configuration file: where it is looked up, what it may contain, how its values
interact with CLI flags, and exactly what happens when it is missing, unreadable,
malformed, or invalid. The README's Configuration section summarizes; where the
two differ, this document wins. Every example below is replayed verbatim by the
fixture — the TOML blocks are parsed by the real loader, the error lines are
produced by the real error path, and the test fails if the implementation, the
fixture, this document, or the README's Configuration summary drift apart.
Changing the contract means changing all of them in one commit.

## Scope

`claude-print` reads **at most one optional TOML file** per invocation. The
file is read only by normal prompt runs; `--help`, `--version`, `--check`,
and `serve` never load it (`main.rs` dispatches `--version`, `--check`, and
`serve` before the config step, and `--help` is answered by clap inside
`Cli::parse()`, before `main()`'s body runs at all). `claude-print` never
creates, writes, or scaffolds the file — you own it entirely. Nothing else
is configurable through this file: relay-hook internals, watchdog budgets
other than `timeout_secs`, output formats, and pool behavior are CLI-only.

That dispatch scope is pinned at the binary level by
`tests/config_entry_point_scope.rs` (bead claudepr-58e1a4b0; the `--help`
arm is bead claudepr-81b1f1d3): each of those
entry points runs with a poisoned config — unreadable, or garbage TOML — at
the discovered path and via `--config`, and must produce byte-identical
output and exit status to a run with no config file present at all.

## File location and path precedence

The path is resolved in this order — the first rule that matches wins:

1. `--config <FILE>` — an explicit path, used exactly as given. It **replaces**
   path discovery entirely (`main.rs` routes `cli.config` through the same
   `Config::load_or_default` call): the discovered file is never read, and
   nothing is ever merged.
2. `$XDG_CONFIG_HOME/claude-print/config.toml` — when `XDG_CONFIG_HOME` is set
   and valid UTF-8. The value is used as given: an **empty-but-set**
   `XDG_CONFIG_HOME` counts as set and yields the cwd-relative path
   `claude-print/config.toml`. A value that is not valid UTF-8 fails the
   `var()` read and falls through to rule 3.
3. `$HOME/.config/claude-print/config.toml` — otherwise, via
   [`get_home`](../src/util.rs)'s strict policy: `HOME` must be set, non-empty,
   an existing directory, and writable (proven by a create-write-remove probe).
   When it is not, path resolution itself fails with the `HOME` error — never a
   `/root`, passwd, or cwd fallback.

```text
error: invalid config: HOME environment variable not set or empty; set HOME to the user's home directory
```

A valid `HOME` is required in every case, even when `XDG_CONFIG_HOME` or
`--config` supplies the path: `main.rs` validates `HOME` process-wide before
dispatching any entry point, because Claude Code state and transcripts live
under `$HOME` regardless of where the config file came from.

## Claude Code state and `CLAUDE_CONFIG_DIR`

The file this contract describes configures the wrapper only. Claude Code's
own state — credentials, `settings.json`, history, and the session
transcripts claude-print reads back — is not configurable through
claude-print at all: it lives under `$HOME/.claude/`, session transcripts
under `$HOME/.claude/projects/<cwd-slug>/<session-id>.jsonl`.

`CLAUDE_CONFIG_DIR` — Claude Code's variable for relocating that state — is
deliberately outside this contract and unsupported, in both directions:

1. claude-print never sets it. The per-run temp directory exists solely for
   the Stop-hook settings injection (`src/hook.rs`) and never redirects the
   config dir.
2. A value inherited from the parent environment is scrubbed from the child
   environment before `execvpe`: `CLAUDE_CONFIG_DIR` is an entry in
   `SCRUBBED_ENV` (`src/pty.rs`), the one list the pre-fork child-env
   builder filters through. Outer wrappers — agent cleanrooms, NEEDLE-style
   sandboxes — export it to relocate Claude Code's whole config dir, and
   everything they spawn inherits it.

Both rules exist because claude-print's transcript readers are HOME-rooted by
construction: the poller's transcript-path derivation
(`derive_transcript_path`, from `session_id` + `cwd`) and the stream-json
live reader's binding (`projects_dir_for_cwd`) both root at `get_home()` and
cannot follow a redirect. A leaked `CLAUDE_CONFIG_DIR` would relocate the
child's transcript while claude-print keeps watching the HOME-rooted tree —
and `scripts/check-billing.sh` inspects the newest transcript under
`~/.claude/projects/`, so a redirected session would also be invisible to
the release billing checks.

Relocating Claude Code state therefore means setting `HOME` — under the
strict resolution of the File location rules above, provisioned per
`docs/notes/home-handling-strategy.md`. No flag, config key, or environment
variable redirects the config dir through claude-print.

The behavioral invariant is enforced by `tests/claude_config_dir_contract.rs`
(bead claudepr-bfe97ce4); this section's wording — and the README's
`### Claude Code state (CLAUDE_CONFIG_DIR)` summary — are pinned against the
implementation by `tests/claude_config_dir_docs_contract.rs` (bead
claudepr-e46458c4): every sentence above must appear in the section, every
cited file must exist, the HOME-rooting claim is replayed behaviorally (both
readers derived under a throwaway `HOME` with a decoy `CLAUDE_CONFIG_DIR`
inherited), and the no-redirection claim is checked against clap's parser
definitions, the closed-world `[defaults]` schema, and `FORCED_ENV`.
Rewording this section means updating the pin in the same commit.

## TOML structure

The file is a TOML document whose recognized root is a single optional
`[defaults]` table holding four optional keys. Every key is shown here at its
shipped default — this exact block is a valid file and is pinned by the
fixture:

```toml
[defaults]
model = "claude-sonnet-4-6" # string
inherit_hooks = true        # bool
max_turns = 30              # integer (u32)
timeout_secs = 3600         # integer (u64)
```

- The `[defaults]` table itself is optional; a bare `[defaults]` line with no
  keys is valid, so partial configurations are fine. An empty file is equally
  valid and equivalent to no file at all.
- Keys **inside `[defaults]`** are closed-world: an unknown key is rejected at
  parse time with an error listing the four valid names (`deny_unknown_fields`
  on the `Defaults` struct). For this file:

```toml
[defaults]
frobnicate = 1
```

the error is:

```text
error: invalid config: unknown-key.toml: TOML parse error at line 2, column 1
  |
2 | frobnicate = 1
  | ^^^^^^^^^^
unknown field `frobnicate`, expected one of `inherit_hooks`, `model`, `max_turns`, `timeout_secs`
```

- Keys and tables **outside `[defaults]`** are silently ignored — the root
  schema accepts `defaults` and disregards everything else. Put typos inside
  the table and they are caught; put them outside and they are dead weight.
  Only one `[defaults]` table may appear: a second one is a TOML duplicate-key
  error, and so is any repeated key inside it.

Both halves of the ignore rule are pinned by the fixture — a root-level key
and a whole foreign table each load fine:

```toml
extra = 1
[defaults]
model = "claude-opus-4-8"
```

```toml
[other]
x = 1
```

## Supported keys, types, and built-in defaults

| Key | Type | Built-in default | CLI counterpart | Meaning |
|-----|------|------------------|-----------------|---------|
| `model` | string | `claude-sonnet-4-6` (`DEFAULT_MODEL` in `src/config.rs`) | `--model`, `-m` | Model forwarded to the child `claude` process |
| `inherit_hooks` | boolean | `true` | `--no-inherit-hooks` | `false` isolates the run from the user's `~/.claude/settings.json` hooks — see the README "Hook inheritance" section |
| `max_turns` | integer (`u32`) | `30` | `--max-turns` | Maximum agentic turns, forwarded to the child |
| `timeout_secs` | integer (`u64`) | `3600` | `--timeout` | claude-print's own wall-clock watchdog (never forwarded to the child) |

## Value resolution (CLI precedence)

For each setting, resolution picks the first tier that has a value — CLI flag,
then config file, then built-in default:

| Setting | Resolution order |
|---------|------------------|
| `model` | `--model`/`-m` → `defaults.model` → `claude-sonnet-4-6` |
| `inherit_hooks` | `--no-inherit-hooks` → `defaults.inherit_hooks` → `true` |
| `max_turns` | `--max-turns` → `defaults.max_turns` → `30` (resolver tiering — see limitation below) |
| `timeout_secs` | `--timeout` → `defaults.timeout_secs` → `3600` (resolver tiering — see limitation below) |

`--model` and `--no-inherit-hooks` have no clap parser default, so their
absence is detectable and the config tier genuinely applies whenever the flag
is absent. `--no-inherit-hooks` is one-directional — it can only suppress
inheritance; when the config says `inherit_hooks = false` there is no flag
that re-enables it for a single run (point `--config` at a file that omits the
key instead).

**Known limitation:** `defaults.max_turns` and `defaults.timeout_secs` are
parsed and validated but currently have no effect on a real invocation. The
clap `default_value`s on `--max-turns` (`30`) and `--timeout` (`3600`) make an
absent flag indistinguishable from an explicitly passed one, so `main.rs`
always passes `Some(cli.max_turns)` / `Some(cli.timeout)` into the resolvers —
the config tier never fires. Control these two with the CLI flags (or, for
NEEDLE, the `invoke` template in `claude-print.yaml`). The keys are accepted
so a future fix can honor them without a format change.

## `--mcp-config`: a config-shaped flag with no config tier

`--mcp-config` is the only long flag other than `--config` whose name contains
"config" (the no-redirection claim above enumerates exactly those two), so its
boundary with this file is worth stating precisely: **it is not a
configuration-file setting and has no config tier.** The closed-world
`[defaults]` schema has no `mcp_config` key — a config file carrying one is
rejected at parse time by the same unknown-field tier as any other stray key
(§"TOML structure"), naming `mcp_config` and the four real keys, exit 2, before
any session starts. There is no `resolve_*` tiering for it either: unlike
`--model` there is no "flag → config → built-in" chain, because absence has no
built-in value to fall back to — it forwards nothing (§ below).

What the flag shapes instead is the **child argv**. When one or more values are
given, `main.rs` carries them into `LaunchOptions::mcp_configs` and
`Session::build_child_argv` emits — after the relay `--settings=` (and after
`--setting-sources=` when isolation mode is on), before the forwarded
`--model`/`--max-turns` — exactly this segment:

```text
--strict-mcp-config --mcp-config <entry> [--mcp-config <entry> …]
```

one `--mcp-config <entry>` pair per entry in entry order, with
`--strict-mcp-config` exactly once and only when the list is non-empty (an
empty list forwards neither flag, leaving the child's own default MCP
resolution in force). The strict flag is the point of the feature (bf-uj0
bound MCP init): only the named configs load, so inherited/project/global MCP
servers that can hang on connect cannot wedge headless startup.

**Accepted spellings.** The flag is repeatable and comma-delimited
(`value_delimiter = ','` in `src/cli.rs`): `--mcp-config a --mcp-config b`,
`--mcp-config a,b`, and `--mcp-config=a,b` all yield the same two entries in
the same order. A value is required — `--mcp-config` with no value is a clap
usage error (exit 2, `a value is required for '--mcp-config <MCP_CONFIG>'`),
answered inside `Cli::parse()` before the config file is ever read. There is
no short form.

**Values are forwarded verbatim — the child is the validator.** claude-print
checks neither that a path exists nor that inline JSON parses; a bad value
surfaces as the child's own startup failure through the ordinary session error
paths (first-output watchdog, `--show-child-stderr`). The only
claude-print-side failure is the argv NUL guard (`Error::Internal`,
`mcp-config value invalid`), unreachable from a shell since NUL cannot cross
`execve`.

**Interaction with this file: none, in both directions.** A config file that
loads (even one actively setting `model` or `inherit_hooks`) leaves the
`--mcp-config` segment of the child argv byte-identical, and a config file
that fails to load still exits 2 with its `invalid config:` error whether or
not `--mcp-config` was passed — the flag neither rescues nor bypasses the
config step.

**Pooled invocations do not use this argv path.** With `--pool-socket` the
acquired worker's `claude` was already launched by the daemon with a fixed
argv (`--settings=` plus `--setting-sources=` only — `pool.rs`'s
`create_worker` never sets MCP configs), so the flag cannot reach the child.
It is inert, never fatal: the prompt still runs on the daemon's launch, and
with `--verbose` the client lists `--mcp-config` in its `not applied:`
diagnostic alongside the other per-invocation launch flags (README §"Warm PTY
pool" documents the same for its siblings).

The forwarding order, the spelling equivalence, the config independence, the
closed-world rejection, and the pool leg are pinned by mock-child argv
recordings (`MOCK_RECORD_ARGS`) in `tests/binary_e2e.rs` and
`tests/pool_socket_e2e.rs` (bead claudepr-af36fc41); the argv shapes are
additionally unit-tested at the source in `src/session.rs`
(`build_child_argv_*`).

## Missing file

A missing config file is not an error: `claude-print` runs on built-in
defaults. `Config::load_or_default` returns `Config::default()` when opening
the path fails with `NotFound`. This holds for the discovered default path
*and* for an explicit `--config <FILE>` that does not exist, since `main.rs`
routes both through the same call. Only a file that exists but cannot be read,
parsed, or validated is fatal.

## Validation

A file that parses is then checked field by field (`Defaults::validate`), in
this order: `model` first, then `max_turns`, then `timeout_secs` — only the
first failure is reported.

| Key | Constraint |
|-----|------------|
| `model` | Non-empty, at most 100 bytes (the length check is `model.len()`, so multi-byte characters count per byte); only alphanumeric characters (Unicode-aware), `-`, `_`, `.`; must start with lowercase `claude-` |
| `max_turns` | 1–1000 inclusive |
| `timeout_secs` | 1–86400 inclusive (24 hours) |
| `inherit_hooks` | TOML boolean only (a type constraint, enforced at parse time) |

Types are strict — values are never coerced. `max_turns = "50"` (quoted),
`max_turns = 50.5`, `max_turns = -10`, `model = 123`, `inherit_hooks = "true"`,
and `timeout_secs = true` all fail at **parse** time, before validation runs,
each reporting the TOML position and the expected type. The first one is this
file:

```toml
[defaults]
max_turns = "50"
```

and fails with:

```text
error: invalid config: max-turns-string.toml: TOML parse error at line 2, column 13
  |
2 | max_turns = "50"
  |             ^^^^
invalid type: string "50", expected u32
```

## Error reporting

If the config file exists but cannot be read, parsed, or validated,
`claude-print` exits with status **2** and never starts a session — it does
not fall back to defaults with a warning. Stdout is empty in every mode; the
error goes to stderr, shaped by the output format (text mode: one `error: …`
line; `json`/`stream-json`: a structured `result` object with
`subtype: "internal_error"` — the same shapes as every other pre-session
failure, specified in `docs/notes/output-format-contracts.md`).

All three failure tiers share the `error: invalid config:` prefix and exit 2;
they differ in what follows. The constraint-violation example below comes from
this file:

```toml
[defaults]
model = "gpt-4"
```

- **Unreadable file** — exists but `read` fails (permissions, or the path is a
  directory):

  ```text
  error: invalid config: cannot read config at config-dir: Is a directory (os error 21)
  ```

- **Parse failure** — malformed TOML, unknown key inside `[defaults]`, a
  duplicate key or table, or a wrong type: reports the TOML line and column
  plus a source excerpt (see the examples above).

- **Constraint violation** — parses, then fails a validation check: reports
  the reason instead of a position. The doubled `invalid config:` is not a
  typo — the per-field reason carries its own prefix, and the path wrapper
  adds another:

  ```text
  error: invalid config: config validation failed at bad-model.toml: invalid config: model name 'gpt-4' must start with 'claude-'
  ```

In `json` and `stream-json` modes the same message travels in the structured
result object written to **stderr** (config errors fire before a session
exists, so stdout stays clean). For a file whose entire content is `[[`
(malformed TOML):

```toml
[[
```

the result object is:

```json
{"claude_version":"2.1.276 (Claude Code)","error_message":"invalid config: malformed.toml: TOML parse error at line 1, column 3\n  |\n1 | [[\n  |   ^\ninvalid key\n","is_error":true,"subtype":"internal_error","type":"result"}
```

For a quick check without touching the default config:

```bash
bad_config="$(mktemp)"
printf '[[\n' > "$bad_config"
claude-print --config "$bad_config" --output-format json "test prompt" 2>config-error.json
status=$? # 2
jq . config-error.json
rm "$bad_config" config-error.json
```

## Message-shaping provenance

The exact strings above are produced by a fixed chain, pinned end-to-end:
`Config::load_or_default` wraps each failure tier (`cannot read config at …`,
`invalid config at …`, `config validation failed at …`), `Error::Config`'s
Display adds the leading `invalid config: `, and `From<Error> for
ClaudePrintError` normalizes `invalid config at <path>` to
`invalid config: <path>` so no message doubles the wrapper the way the
validation tier deliberately does. `emit_error` renders the final line or
JSON object. The fixture replays this chain through the real functions; if any
link changes its wording, the pin fails.
