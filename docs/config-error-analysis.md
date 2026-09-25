# Config Error Handling Analysis

**Date:** 2026-08-15 (original) · **Revised:** 2026-09-24 — reconciled with the
current dispatch behavior (serve entry point, process-wide `HOME` preflight,
missing-file-is-not-an-error semantics)
**Project:** claude-print
**Purpose:** Document the actual current behavior of config parsing and error handling

> **Normative sources.** The user-facing contract is
> [`docs/notes/config-file-contract.md`](notes/config-file-contract.md)
> (pinned by `tests/config_contract.rs`); the README's Configuration section
> summarizes it. This document is the engineering analysis behind that
> behavior — where the three disagree, the contract wins.

## Executive Summary

**Finding:** Config errors are handled with **NO silent fallback for a file
that exists**. A config file that exists but cannot be read, parsed, or
validated is an immediate, visible, exit-2 failure before any session starts.
A config file that does not exist is a **defined non-error**: the run proceeds
on built-in defaults. That missing-file case is a deliberate part of the
contract ("at most one *optional* TOML file"), not a swallowed error.

**Key facts about the current dispatch:**

- Config loading happens **only on the ordinary prompt path**, after prompt
  resolution. `--help`, `--version`, `--check`, and `serve` never load it.
- Two separate validations run at startup and must not be conflated:
  a **process-wide `HOME` preflight** (`src/main.rs`, before any entry-point
  dispatch) and the **config-file load** (`Config::load_or_default`,
  prompt path only). See the next section.
- Exit code 2 (setup error) is used for all config failures; JSON and
  stream-json modes receive the structured `result` object on **stderr**
  (stdout stays clean — config errors fire before a session exists).

## Two separate validations: `HOME` preflight vs config-file loading

The 2026-08-15 version of this analysis attributed the `HOME` failure to
`Config::default_path()`. Today that is only true at library level. The CLI
validates `HOME` **process-wide, before dispatching any entry point**
(`src/main.rs`, immediately after `Cli::parse()` and the orphan sweep):

```rust
// HOME is a process-wide prerequisite, including for early-exit entry
// points such as --version. Validate it before dispatch so the CLI,
// config, poller, and direct Session callers share one strict contract.
if let Err(error) = claude_print::util::get_home() {
    // emit_error (structured in json/stream-json, "unknown" claude_version)
    exit_with_cleanup(2);
}
```

`get_home()` (`src/util.rs`) enforces the strict policy: `HOME` must be set,
non-empty, an existing directory, and writable (proven by a
create-write-remove probe). There is never a `/root`, passwd, or cwd
fallback. The rationale is not the config file at all: Claude Code state,
trust state, and transcripts live under `$HOME` regardless of where the
config file came from — which is why even `--config <FILE>` or a complete
`XDG_CONFIG_HOME` does not exempt an invocation (`tests/home_unset.rs::`
`cli_home_unset_xdg_config_and_transcript_discovery_share_strict_error_contract`).

**Config-file loading**, by contrast, runs later and only for prompt runs
(`src/main.rs`, after prompt resolution and the NUL-byte check):

```rust
let config = match cli
    .config
    .clone()
    .map_or_else(Config::default_path, Ok)
    .and_then(|path| Config::load_or_default(&path))
{ Ok(config) => config, Err(e) => { /* emit_error; exit_with_cleanup(2) */ } };
```

`Config::default_path()` (`src/config.rs`) still fails with a `HOME` error
when `XDG_CONFIG_HOME` is unset and `HOME` is invalid — pinned at library
level by `tests/config_contract.rs` — but from the CLI that arm is
effectively shadowed: the preflight has just validated the same condition
through the same `get_home()` call, so an invocation that reaches
`default_path()` already has a usable `HOME`. The two failures also render
differently: the preflight emits a `Setup` error whose message is the
actionable `HOME …` guidance, while a config failure emits a
`ClaudePrintError::Config` (see "Error reporting" below).

## Entry-point dispatch (current behavior)

`main()` dispatches in this order; the cited symbols (`src/main.rs`,
`src/cli.rs`, `src/config.rs`, `src/check.rs`, `src/error.rs`,
`src/emitter.rs`, `src/util.rs`) are the stable anchors — line numbers are
deliberately omitted, they rotted once already.

```
main()
  │
  ├─ Cli::parse()                      ← clap handles --help HERE (exit 0,
  │                                      before main()'s body runs anything)
  ├─ hook::cleanup_orphans()           ← skipped when --check (check owns its scan)
  ├─ get_home() preflight              ← ALL entry points below validate HOME
  ├─ --version / -V                    ← exit 0 (one probe of the claude binary)
  ├─ --check [--clean]                 ← exit 0 all-pass / 2 any-fail
  ├─ which(claude_bin)                 ← shared by serve and prompt runs, exit 2
  ├─ Command::Serve                    ← run_serve(); NEVER RETURNS: never loads
  │                                      config, never validates a prompt
  └─ prompt path only:
       ├─ prompt resolution            ← exit 4 no-prompt / bad --input-file
       ├─ NUL-byte check               ← exit 2
       ├─ config load                  ← ERRORS CAUGHT HERE (exit 2)
       └─ session::run / run_pooled    ← exit 0 / 1 / 2 / 124 / 130
```

| Entry point | `HOME` preflight | Invokes the `claude` binary? | Loads the config file? | Exit codes |
|---|---|---|---|---|
| `--help` | **No** — clap exits during `Cli::parse()`, before the preflight | Never, not even a probe | Never | 0 |
| `--version` / `-V` | Yes | Exactly once, as the documented `<bin> --version` probe (non-fatal; degrades to a `not found` clause) | Never | 0 (2 if `HOME` invalid) |
| `--check` [`--clean`] | Yes | Probes it (PATH or `--claude-binary`) as one of the checks | Never | 0 all checks pass, 2 otherwise |
| `serve` | Yes | `which` check up front; workers are spawned with this binary | **Never** — dispatched before prompt/config state | 0 clean signal shutdown, 2 setup or accept failure |
| prompt run | Yes | `which` check; then the session child | Yes — after prompt resolution | 0 / 1 / 2 / 4 / 124 / 130 |

### `--help`

Handled entirely inside `Cli::parse()`: clap prints the full help text to
stdout (empty stderr) and exits 0 during parsing, before any of `main()`'s
body — including the `HOME` preflight — executes. It is therefore the one
entry point that works without a valid `HOME` and never invokes the claude
binary at all. Pinned by `tests/help_version_e2e.rs::`
`help_exit0_text_on_stdout_stderr_empty` (claudepr-be05847d).

### `--version` / `-V`

clap's built-in version flag is disabled (`disable_version_flag = true` in
`src/cli.rs`); `--version`/`-V` is a custom flag handled in `main()` **after**
the `HOME` preflight. It runs `resolve_claude_version()` — one subprocess
spawn of `<claude-binary> --version` — prints `version_string(...)` to stdout,
and exits 0. The probe failing is non-fatal (the version line degrades to a
`not found` clause). Without a valid `HOME`, `--version` exits 2 with the
actionable `HOME` message and no version output — pinned by
`tests/home_unset.rs::env_u_home_version_fails_with_actionable_error` and
`nonexistent_home_version_fails_without_root_fallback`.

### `--check` [`--clean`]

Handled after `--version`. The orphan sweep is skipped for `--check` (check
mode owns its scan; only `--check --clean` removes the directories it
reports). It runs `check::run_with_clean(cli.claude_binary, cli.clean)`
(`src/check.rs`), which probes the claude binary, openpty, mkfifo, the mock
PTY round-trip when available, and the billing env contract, then returns
0 when every probe passed and 2 otherwise. The config file is never
consulted.

### `serve`

The ADR-005 warm-PTY-pool daemon. `main()` matches `Command::Serve` after the
`HOME` preflight, `--version`, `--check`, and the shared binary-existence
check — the pool spawns `claude` workers with this binary, so entering serve
without it would just churn failing warmups — but **before** prompt
resolution and config loading: prompt/stdin/config handling is client-path
state the pool does not use, and `run_serve()` never returns. Consequently
`serve` never reads the config file and never falls through to prompt
validation (a fall-through regression would exit 4 with "no prompt
provided"). Setup failures — `--pool-size` of 0 or above 256, an unbindable
socket path — exit 2 before any worker is spawned; `SIGINT`/`SIGTERM` tear
down every worker, remove the socket, and exit 0 (a supervisor stopping the
service is not a failure). Pinned by
`tests/serve.rs::serve_dispatch_enters_the_server_path_and_never_validates_a_prompt`,
`serve_rejects_missing_claude_binary_before_binding`, and
`default_non_serve_invocation_is_unchanged`, plus the parse-level pins in
`src/main.rs`'s unit tests (claudepr-1feb2d1b).

## Config loading on the prompt path

`Config::load_or_default` (`src/config.rs`) has three tiers and one
deliberate non-error:

```rust
let contents = match std::fs::read_to_string(path) {
    Ok(contents) => contents,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
        return Ok(Config::default());   // missing file IS NOT an error
    }
    Err(e) => return Err(/* "cannot read config at {path}: {e}" */),
};
let config: Config = toml::from_str(&contents)
    .map_err(/* "invalid config at {path}: {toml error}" */)?;
if let Some(ref defaults) = config.defaults {
    defaults.validate().map_err(/* "config validation failed at {path}: {reason}" */)?;
}
```

1. **Read** — exists but unreadable (permissions, or the path is a
   directory): `cannot read config at …`.
2. **Parse** — malformed TOML, an unknown key inside `[defaults]`
   (`deny_unknown_fields`), a duplicate key/table, or a wrong type: reports
   the TOML line/column and a source excerpt.
3. **Validate** (`Defaults::validate`, in order `model` → `max_turns` →
   `timeout_secs`, first failure wins): parses, then fails a constraint
   (e.g. `model name 'gpt-4' must start with 'claude-'`).

Validation constraints: `model` non-empty, ≤100 bytes, alphanumeric + `-`/`_`/`.`,
must start with lowercase `claude-`; `max_turns` 1–1000; `timeout_secs`
1–86400; `inherit_hooks` boolean (parse-time type constraint).

The function name `load_or_default` is still mildly misleading, but its
doc-comment now states the contract exactly: default **values for missing
optional fields within a valid (or absent) file** — never a fallback when a
file that exists is invalid. A separate `Config::load` exists for callers
that want a missing file to be an error.

## Error reporting

A config failure becomes `ClaudePrintError::Config` via
`From<Error> for ClaudePrintError` (`src/error.rs`), which also normalizes
the wrapper `invalid config at <path>` to `invalid config: <path>` (the
validation tier deliberately keeps a doubled prefix — the per-field reason
carries its own). `emit_error` (`src/emitter.rs`) then renders:

- **Text mode:** one `error: invalid config: …` line on **stderr**; stdout
  empty; exit 2.
- **`json` / `stream-json`:** a structured `result` object on **stderr**
  (`emit_error` routes `ClaudePrintError::Config` to stderr specifically —
  config errors fire before a session exists, so stdout, which carries the
  response payload, stays clean); exit 2; `subtype: "internal_error"`.

```json
{"claude_version":"2.1.276 (Claude Code)","error_message":"invalid config: malformed.toml: TOML parse error at line 1, column 3\n  |\n1 | [[\n  |   ^\ninvalid key\n","is_error":true,"subtype":"internal_error","type":"result"}
```

Every example line in this section is replayed verbatim through the real
loader and emitter by `tests/fixtures/config_contract_examples_v1.json` (see
`tests/config_contract.rs`); the exact strings cannot drift without failing
that test.

## Exit code mapping

| Error type | Exit code | Rationale |
|------------|-----------|-----------|
| `HOME` preflight failure (any entry point but `--help`) | 2 | Setup failure (process-wide prerequisite) |
| Config error (unreadable / unparseable / invalid) | 2 | Setup failure (misuse of tool/config) |
| Missing config file | — (not an error) | Runs on built-in defaults |
| Binary not found (`which`) | 2 | Setup failure (missing prerequisite) |
| No prompt / unreadable `--input-file` or stdin | 4 | Input error |
| serve: `--pool-size` or socket setup failure, accept-loop failure | 2 | Setup failure, before/without workers |
| serve: SIGINT/SIGTERM shutdown | 0 | A supervisor stopping the service is not a failure |
| Timeout | 124 | GNU timeout convention |
| Interrupted (SIGINT/SIGTERM) on a session | 130 | Git/SIGINT convention (128 + 2) |
| Assistant error | 1 | Generic failure (Claude failed, not claude-print) |

## What changed since the 2026-08-15 analysis

1. **Missing file is no longer an error.** The original analysis' "Test 3"
   asserted a missing config file exits 2 with `config file not found`.
   Today `load_or_default` returns `Config::default()` on `NotFound`
   (`src/config.rs`; unit test
   `load_or_default_returns_defaults_when_file_missing`). The contract makes
   the file explicitly optional; only a file that exists and fails to
   read/parse/validate is fatal.
2. **JSON/stream-json config errors go to stderr, not stdout.** The original
   claimed the structured object lands on stdout. `emit_error` special-cases
   `ClaudePrintError::Config` to stderr so pre-session stdout stays clean
   (`tests/config_startup_errors.rs::`
   `malformed_config_json_error_is_structured_on_stderr`).
3. **`serve` exists and never loads the config file.** Not present in the
   original analysis (predates ADR-005). See the dispatch table above.
4. **The `HOME` preflight is process-wide and pre-dispatch.** The original
   attributed `HOME` failures to config path resolution; on the CLI the
   visible failure is the preflight, which fires for `--version`, `--check`,
   and `serve` too (and for `--config`/`XDG_CONFIG_HOME` runs, which never
   consult `HOME` for the *path*). `--help` is the only exemption because
   clap exits during parsing.
5. **`Config::default_path` prefers `XDG_CONFIG_HOME`.** Path precedence is
   now `--config <FILE>` → `$XDG_CONFIG_HOME/claude-print/config.toml` →
   `$HOME/.config/claude-print/config.toml`.
6. **`--check` grew `--clean`** (`check::run_with_clean`) and skips the
   automatic orphan sweep.
7. **`--version` is a custom flag** (clap's built-in disabled), still
   dispatched after the preflight, with one non-fatal probe of the claude
   binary.
8. **Stale line numbers refreshed** — the original cited `main.rs:186-221`
   for config loading and `main.rs:47-58` for the early-exit flags, which no
   longer matched the file.

## Regression tests

| Test | Pins |
|------|------|
| `tests/config_contract.rs` | The normative contract end-to-end: path precedence (incl. empty-but-set and non-UTF-8 `XDG_CONFIG_HOME`, strict `HOME` failure), loader tiers through the real `Config::load_or_default`, and every documented error line byte-for-byte through `emit_error`; keeps `docs/notes/config-file-contract.md` and the README from drifting (bead claudepr-227efdb1) |
| `tests/config_startup_errors.rs` | Binary-level: a malformed config is visible in text mode and structured on **stderr** in `json`/`stream-json`, exit 2, stdout clean |
| `tests/config_parse_errors.rs` | Parse-tier failures (unclosed bracket/string, …): exit 2, structured JSON error, no silent fallback to defaults (bead claudepr-ea80e6b2) |
| `tests/home_unset.rs` | The `HOME` preflight: unset/empty/missing/read-only `HOME` all exit 2 with the same actionable message by every entry point (`env_u_home_version_fails_with_actionable_error`, `assert_cli_home_unset_error` callers), no `/root` fallback under a real `chroot`, and `XDG_CONFIG_HOME`/`--config` not exempting a run |
| `tests/help_version_e2e.rs` | `--help` exit 0 with help on stdout and no claude invocation at all; `--version`/`-V` exit 0 with exactly one probe invocation; neither flag starts session machinery or loads a prompt (claudepr-be05847d) |
| `tests/serve.rs` | Serve dispatch: `serve_dispatch_enters_the_server_path_and_never_validates_a_prompt` (no fall-through to prompt validation, hence no config load), `serve_rejects_missing_claude_binary_before_binding`, `default_non_serve_invocation_is_unchanged`, plus pool-size/socket setup failures exiting 2 and clean signal shutdown exiting 0 |
| `src/main.rs` (unit) | Parse-level serve pins: `serve` parses as `Command::Serve` with compiled-in defaults, flags carry through, a plain prompt never dispatches serve, a positional after `serve` is a parse error (claudepr-1feb2d1b) |
| `src/config.rs` (unit) | Loader semantics: missing-file-returns-defaults, per-tier error wording, validation ranges and first-failure ordering, `default_path` XDG/HOME precedence |

## Conclusion

The current behavior is correct, robust, and now pinned by tests at every
tier:

1. ✅ No silent fallback for a file that exists — read/parse/validation
   failures are hard, visible, exit-2 errors
2. ✅ A missing file is a defined non-error (built-in defaults), per contract
3. ✅ `HOME` is validated once, process-wide, before any dispatch —
   including the early-exit entry points — and `--help` alone is exempt
4. ✅ Proper exit codes (2 for setup/config, 4 for input, 124/130/1 for the
   session-path outcomes; serve: 0 clean shutdown, 2 setup failure)
5. ✅ Structured output in JSON modes, on stderr so stdout stays clean
6. ✅ Validation happens before any session starts

Of the original recommendations, the `load_or_default` doc-comment and the
integration-test coverage have shipped; the optional rename (`load`,
`load_strict`, …) remains open and is now low-value — the contract note and
the doc-comment carry the semantics, and a rename would churn the pinned
messages for no behavioral gain.
