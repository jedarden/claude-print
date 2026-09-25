# Configuration-File Contract

| | |
|---|---|
| **Contract version** | v1 |
| **Pinned by** | `tests/config_contract.rs` against `tests/fixtures/config_contract_examples_v1.json`; the README's Configuration-section summary (key table, shipped defaults, path precedence) by the same test (bead claudepr-746dd1c4), and the engineering analysis `docs/config-error-analysis.md`'s contract-bearing excerpts (quoted examples, path precedence and XDG edges, resolution limitation) likewise (bead claudepr-09637c58) |
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
file is read only by normal prompt runs; `--version`, `--check`, and `serve`
never load it (`main.rs` dispatches those entry points before the config
step). `claude-print` never creates, writes, or scaffolds the file — you own
it entirely. Nothing else is configurable through this file: relay-hook
internals, watchdog budgets other than `timeout_secs`, output formats, and
pool behavior are CLI-only.

That dispatch scope is pinned at the binary level by
`tests/config_entry_point_scope.rs` (bead claudepr-58e1a4b0): each of those
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
