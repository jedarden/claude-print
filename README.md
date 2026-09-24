# claude-print

Drop-in replacement for `claude -p` (print/headless mode) that drives the Claude Code interactive TUI via PTY — keeping sessions on the unlimited subscription pool rather than the per-token Agent SDK credit pool.

## Why this exists

Anthropic routes `claude -p` (headless/SDK mode) through a separate Agent SDK credit pool ($100–$200/month on Max plans). Only the interactive TUI (`cc_entrypoint=cli`) draws from the unlimited subscription.

The billing path is determined by an `isatty` check inside the `claude` binary: when stdout is a TTY, the session is tagged `cc_entrypoint=cli` and billed against the subscription. When stdout is a pipe (as with `claude -p`), it becomes `cc_entrypoint=sdk-cli` and draws from the credit pool instead.

`claude-print` allocates a PTY, drives the interactive TUI over it, auto-dismisses the trust dialog, injects the user prompt via bracketed paste, waits for the Stop hook via a FIFO, reads the JSONL transcript, and emits clean stdout output — giving callers `claude -p` wire-compatible output while billing against the subscription.

## Prerequisites

- **Claude Code** must be installed and authenticated. See [claude.ai/code](https://claude.ai/code).
- An active **Claude subscription** (Pro or Max plan) is required. The whole point is to bill against subscription, not credits.
- **`HOME` must be set to a non-empty value** for the user running `claude-print`. Claude Code configuration, trust state, and transcripts are resolved beneath this directory.
- Linux only. PTY support requires POSIX — no Windows ConPTY.

## Install

```bash
sh install.sh
```

`install.sh` downloads a pre-built static musl binary from GitHub Releases (`jedarden/claude-print`) — the supported distribution channel for release artifacts — runs `--check` to verify the setup, and copies `claude-print.yaml` to `~/.needle/agents/` if NEEDLE is present. Note that GitHub Releases is the artifact host, not the source of truth for the code; see [Repository & contributions](#repository--contributions).

Every downloaded artifact is verified against the release's published `sha256sums.txt` before it is installed or executed. A missing manifest, an asset with no checksum entry, or any digest mismatch aborts the install with nothing placed — the check fails closed. The `mock_claude` fixture remains optional: a release whose manifest does not list it skips the fixture instead of failing.

Set `SKIP_MOCK_CLAUDE=1` to skip the `mock_claude` test fixture download.

Release artifacts are published only to GitHub Releases — Forgejo hosts no
release assets — so the default download URL is a GitHub URL. If GitHub is
unreachable from the installing host, set `CLAUDE_PRINT_RELEASE_URL` to a
base URL serving that release's assets (`sha256sums.txt`,
`claude-print-x86_64-linux`, and the rest); `install.sh` runs the identical
checksum verification against whichever host serves them, so a mirror can
redistribute the artifacts but cannot bypass verification. With no mirror
reachable, [build from source](#build-from-source) from the canonical Forgejo
repository — source availability never depends on the mirror.

### Repository & contributions

`git.ardenone.com/jedarden/claude-print` (Forgejo) is the canonical repository and the destination for all pushes. The GitHub repo (`jedarden/claude-print`) is a read-only push mirror — do not treat it as authoritative, and expect it to always reflect Forgejo rather than the reverse. "Read-only" describes source and refs: mirror syncs carry Forgejo → GitHub and never the reverse. Release artifacts run the other way conceptually — they are published only to GitHub Releases, and Forgejo hosts no release assets. The canonical publication path is one-directional: a `vX.Y.Z` tag is pushed to Forgejo first, the `claude-print-ci` Argo Workflow builds the tagged commit from Forgejo, and the workflow publishes the artifacts to GitHub Releases, from which nothing flows back. That makes GitHub Releases the supported download path for `install.sh`, with `CLAUDE_PRINT_RELEASE_URL` redirecting the installer to any host serving the same assets when GitHub is unreachable (see [Install](#install)). Contribute by pushing to Forgejo directly, or by opening an issue or PR on either host — mirror-side PRs are applied on Forgejo before merge.

### Build from source

```bash
git clone https://git.ardenone.com/jedarden/claude-print  # canonical (Forgejo); the GitHub repo is a read-only mirror
cd claude-print
cargo build --release

# fully static binary (recommended for deployment):
cargo build --target x86_64-unknown-linux-musl --release
```

Building from source is also the install fallback whenever GitHub Releases is
unreachable — nothing in this path touches the GitHub mirror.

The binaries land under Cargo's target directory, which is not always
`./target` — so don't assume that path when you go to run them:

- **Stock Cargo checkout:** `target/release/claude-print` and
  `target/x86_64-unknown-linux-musl/release/claude-print`.
- **Hosts with the shared `cargo` wrapper** (`~/.local/bin/cargo` on this
  fleet's codinghome and lab boxes): the wrapper force-sets
  `CARGO_TARGET_DIR=/build/<repo>` for every invocation — here
  `/build/claude-print/release/claude-print` and
  `/build/claude-print/x86_64-unknown-linux-musl/release/claude-print` — and
  refuses a `--target-dir` outside that directory. `./target/` is never
  created.

Resolve the directory through cargo instead of hardcoding either location —
`cargo metadata` goes through the same wrapper, so it reports the redirected
directory where one is enforced and the checkout's `target` dir elsewhere:

```bash
TARGET="$(cargo metadata --no-deps --format-version 1 | jq -r .target_directory)"
"$TARGET/release/claude-print" --version
```

`cargo run --bin claude-print -- --check` runs the smoke check directly from
the build and works under both layouts.

Architectures: `x86_64` only (static musl binary). aarch64 / ARM Linux is out of scope for v1.0 — see `docs/plan/plan.md` Non-Goals. CI builds only for the x86_64 runner; an `install.sh` aarch64 branch would 404 because no such release asset is produced.

## Self-check

After install, verify the PTY, FIFO, and billing env-input mechanics:

```bash
claude-print --check
```

This is credential-free and runs no session. Its billing row confirms the
binary still forces `CLAUDE_CODE_ENTRYPOINT=cli` into the child environment
(env input) even when the parent inherited `sdk-cli`. It does **not** read a
session transcript — confirming `entrypoint: cli` in the JSONL (the billing
evidence) is `./scripts/check-billing.sh`, below. `install.sh` runs `--check`
automatically, but it's worth running manually after upgrades.

## Usage

```
claude-print [OPTIONS] [PROMPT]
claude-print serve [OPTIONS]
```

Reads the prompt from a positional argument, `--input-file`, or stdin (when not a TTY). These are mutually exclusive. The `serve` subcommand runs the ADR-005 warm PTY pool daemon instead of a session — see [Warm PTY pool](#warm-pty-pool-adr-005).

### Examples

```bash
# Positional prompt
claude-print "Summarize this in one sentence"

# Stdin pipe
echo "what is the capital of France?" | claude-print

# File input
claude-print --input-file prompt.txt

# Specify a model
claude-print --model claude-opus-4-8 "Write a haiku about Rust"

# JSON output
claude-print --output-format json "what is 2+2?" | jq .result

# Stream-JSON — real-time JSONL event replay
claude-print --output-format stream-json "Write a story"

# Agentic task with tool use
claude-print --max-turns 5 "List files in current dir and summarize"

# Short timeout for quick questions
claude-print --timeout 30 "quick question"
```

## Flags

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `[PROMPT]` | | | Prompt string (mutually exclusive with `--input-file` and stdin) |
| `--input-file <FILE>` | `-f` | | Read prompt from file |
| `--model <MODEL>` | `-m` | `claude-sonnet-4-6` | Model to use |
| `--max-turns <N>` | | `30` | Maximum agentic turns |
| `--output-format <FORMAT>` | `-o` | `text` | Output format: `text`, `json`, `stream-json` |
| `--allowedTools <LIST>` | | | Comma-separated list of allowed tools |
| `--disallowedTools <LIST>` | | | Comma-separated list of disallowed tools |
| `--dangerously-skip-permissions` | | | Skip permission prompts (dangerous) |
| `--timeout <SECS>` | | `3600` | Wall-clock timeout in seconds |
| `--first-output-timeout <SECS>` | | `90` | First-output timeout in seconds (PTY output) |
| `--stream-json-timeout <SECS>` | | `90` | Stream-json first-output timeout in seconds |
| `--stop-hook-timeout <SECS>` | | `120` | Stop hook watchdog timeout in seconds |
| `--claude-binary <PATH>` | | PATH lookup | Path to claude binary |
| `--pool-socket <PATH>` | | | Acquire a prewarmed worker from the pool daemon at this Unix socket instead of spawning a fresh `claude`; falls back to the ordinary stateless session when the pool can't serve — see [Warm PTY pool](#warm-pty-pool-adr-005) |
| `--config <FILE>` | | XDG or user config | Read configuration from an explicit TOML file |
| `--no-inherit-hooks` | | off | Isolate this run from your `~/.claude/settings.json` hooks by forwarding `--setting-sources=` to the child; the relay hooks claude-print needs always stay active — see [Hook inheritance](#hook-inheritance-inherit_hooks) |
| `--mcp-config <MCP_CONFIG>` | | | MCP config (path or inline JSON) to load; may be repeated or comma-separated for multiple files. Headless runs always pass `--strict-mcp-config` to the child, so only configs named here load — inherited/project/global MCP servers cannot wedge startup |
| `--pretrust-cwd` | | off | Pre-grant folder trust for the working dir by writing `hasTrustDialogAccepted: true` into `~/.claude.json` before spawning the child — the only way to keep the one-time trust dialog from stalling an untrusted cwd without relying on the PTY keyword scanner. Off by default to avoid mutating the shared user config under fleet concurrency; enable it when you have seen trust-dialog stalls |
| `--show-child-stderr` | | off | Surface the child's captured PTY output to stderr when startup is slow or stalls (watchdog first-output timeout, or the prompt was never injected) — useful for diagnosing MCP/init wedges |
| `--verbose` | | | Write timing traces to stderr |
| `--check` | | | Run installation self-test and exit |
| `--clean` | | | With `--check`, remove orphaned temp directories older than one hour |
| `--version` | `-V` | | Print version and exit |
| `--help` | `-h` | | Print help |

## Configuration

### File location

`claude-print` reads at most one optional TOML file. The path is resolved in
this order — the first rule that matches wins:

1. `--config <FILE>` — an explicit path, used as given
2. `$XDG_CONFIG_HOME/claude-print/config.toml` — when `XDG_CONFIG_HOME` is set
3. `$HOME/.config/claude-print/config.toml` — otherwise

An explicit `--config` replaces the default path entirely; it does not merge
with it. A valid `HOME` is required in every case, even when `XDG_CONFIG_HOME`
or `--config` supplies the path (see
[Troubleshooting](#home-in-containers-and-chroots) for why). The config file is
read only by normal prompt runs — `--version`, `--check`, and `serve` never
load it.

`claude-print` never creates or writes the file; you own it.

### Keys

The schema is the `Defaults` struct in `src/config.rs`: one optional
`[defaults]` table holding four optional keys. Every key is shown here at its
shipped default:

```toml
[defaults]
model = "claude-sonnet-4-6" # string
inherit_hooks = true        # bool
max_turns = 30              # integer (u32)
timeout_secs = 3600         # integer (u64)
```

The table is optional too — a bare `[defaults]` line is valid, so partial
configurations are fine. The four keys are:

| Key | Type | Built-in default | CLI counterpart | Meaning |
|-----|------|------------------|-----------------|---------|
| `model` | string | `claude-sonnet-4-6` (`DEFAULT_MODEL` in `src/config.rs`) | `--model`, `-m` | Model forwarded to the child `claude` process |
| `inherit_hooks` | bool | `true` | `--no-inherit-hooks` | `false` isolates the run from your `~/.claude/settings.json` hooks by forwarding `--setting-sources=` to the child — see [Hook inheritance](#hook-inheritance-inherit_hooks) |
| `max_turns` | integer (`u32`) | `30` | `--max-turns` | Maximum agentic turns, forwarded to the child |
| `timeout_secs` | integer (`u64`) | `3600` | `--timeout` | claude-print's own wall-clock watchdog (never forwarded to the child) |

Unknown keys are rejected at parse time; the error lists the four valid names.

### Precedence

Resolution has two tiers: which *file* is read, then how each *value* is
chosen.

**Which file:** `main.rs` loads exactly one file. An explicit `--config <FILE>`
replaces path discovery entirely — the discovered file is never read, so
nothing is merged. Otherwise `Config::default_path` (`src/config.rs`) prefers
`$XDG_CONFIG_HOME/claude-print/config.toml` over
`$HOME/.config/claude-print/config.toml`. Either way,
`Config::load_or_default` (`src/config.rs`) reads that single path.

**Which value:** for each setting, the `Config::resolve_*` functions
(`src/config.rs`) pick the first tier that has a value — CLI flag, then config
file, then built-in default:

| Setting | Resolution order |
|---------|------------------|
| `model` | `--model` → `defaults.model` → `claude-sonnet-4-6` (`resolve_model`) |
| `inherit_hooks` | `--no-inherit-hooks` → `defaults.inherit_hooks` → `true` (`resolve_inherit_hooks`) |
| `max_turns` | `--max-turns` (built-in default `30`); `defaults.max_turns` is not consulted |
| `timeout` | `--timeout` (built-in default `3600`); `defaults.timeout_secs` is not consulted |

**Known limitation:** `defaults.max_turns` and `defaults.timeout_secs` are
parsed and validated but currently have no effect. The clap `default_value`s
on `--max-turns` (`30`) and `--timeout` (`3600`) in `src/cli.rs` make an
absent flag indistinguishable from an explicitly passed one, so `main.rs`
always passes `Some(cli.max_turns)` / `Some(cli.timeout)` into
`Config::resolve_max_turns` and `Config::resolve_timeout_secs`
(`src/config.rs`) — the config tier of those resolvers never fires. Control
these two with the CLI flags (or, for NEEDLE, the `invoke` template in
`claude-print.yaml`). The config keys are accepted so a future fix can honor
them without a format change. `--model` and `--no-inherit-hooks` have no
parser default, so their absence is detectable and their config values apply
whenever the flag is absent.

### Hook inheritance (`inherit_hooks`)

The child `claude` process loads its settings from two independent channels,
and this key controls only the first:

1. **Standard settings sources** — your user `~/.claude/settings.json`, the
   project's `.claude/settings.json`, and `.claude/settings.local.json`. Every
   hook defined there (`SessionStart`, `PreToolUse`, `Stop`, …) fires when its
   event happens.
2. **claude-print's relay settings** — a private settings file in a per-run
   temp dir, forwarded to the child as `--settings`. It carries only the
   `Stop` and `UserPromptSubmit` relay hooks claude-print itself needs (Stop
   detection and transcript binding); none of your settings are in it.

With `inherit_hooks = true` (the default), claude-print forwards no
`--setting-sources` flag, so the child loads both channels exactly as
`claude -p` does: your hooks fire alongside the relay hooks.

With `inherit_hooks = false` — equivalent to passing `--no-inherit-hooks` —
claude-print forwards `--setting-sources=` (empty value). That suppresses the
standard sources, so **your hooks never fire**, while the relay settings stay
active: the empty spelling is measured to suppress the standard sources and
still load the `--settings` file (claude 2.1.270, re-confirmed on 2.1.281 —
`docs/notes/claude-contract-probes.md`, OQ-2). The relay hooks therefore
outrank isolation in every mode, and deliberately so: claude-print's Stop
detection depends on them, which is why no setting can turn them off.
Isolation is for runs whose surroundings make your hooks a liability — a
collector that shouldn't see headless sessions, hooks that prompt or chatter,
NEEDLE workers suppressing hook noise.

Precedence for this key, first match wins:

1. `--no-inherit-hooks` on the command line
2. `defaults.inherit_hooks` in the config file
3. built-in default: `true`

The CLI flag is one-directional — it can only suppress. When the config file
says `inherit_hooks = false` there is no flag that re-enables inheritance for
a single run; point `--config` at a file that omits the key (or sets it
`true`) for that invocation instead.

One caveat: pooled workers (the `serve` daemon, ADR-005) are always launched
isolated regardless of this key — per-invocation flags cannot reach an
already-running worker, and the daemon launches every worker with
`--setting-sources=`. See [Warm PTY pool](#warm-pty-pool-adr-005).

### Missing file

A missing config file is not an error: `claude-print` runs on built-in
defaults. `Config::load_or_default` (`src/config.rs`) returns `Config::default()`
when opening the path fails with `NotFound` — pinned by the
`load_or_default_returns_defaults_when_file_missing` test. This holds for the
default path *and* for an explicit `--config <FILE>` that does not exist, since
`main.rs` routes both through the same call. Only a file that exists but
cannot be read, parsed, or validated is fatal.

### Validation

A file that parses is then checked field by field (`Defaults::validate` in
`src/config.rs`): `model` first, then `max_turns`, then `timeout_secs` — only
the first failure is reported.

| Key | Constraint |
|-----|------------|
| `model` | Non-empty, at most 100 bytes (the length check is `model.len()`, so multi-byte characters count per byte); only alphanumeric characters (Unicode-aware), `-`, `_`, `.`; must start with lowercase `claude-` |
| `max_turns` | 1–1000 |
| `timeout_secs` | 1–86400 (24 hours) |
| `inherit_hooks` | Must be a TOML boolean |

Types are strict: `max_turns = "50"` (quoted), `max_turns = 50.5`, or
`model = 123` are all rejected — values are not coerced. A wrong type fails at
parse time with `invalid type: string "50", expected u32`, before validation
runs.

Unknown keys are rejected at parse time (`deny_unknown_fields` on `Defaults`)
with an error naming the four expected fields:

```text
error: invalid config: unknown-key.toml: TOML parse error at line 2, column 1
  |
2 | frobnicate = 1
  | ^^^^^^^^^^
unknown field `frobnicate`, expected one of `inherit_hooks`, `model`, `max_turns`, `timeout_secs`
```

### Errors

If an existing config file cannot be read, parsed, or validated,
`claude-print` exits with status 2 and never starts a session — it does not
fall back to defaults with a warning. Stdout is empty in every mode; the error
goes to stderr, shaped by the output format.

- **`text` mode:** one line on stderr (stdout empty):

  ```text
  error: invalid config: config validation failed at bad-model.toml: invalid config: model name 'gpt-4' must start with 'claude-'
  ```

  The doubled `invalid config:` is not a typo — the per-field reason carries
  its own prefix, and the path wrapper adds another.

- **`json` / `stream-json` mode:** stdout stays empty and a structured `result`
  object (exit code still 2, subtype `internal_error` for every config
  failure) is written to **stderr** — config errors fire before a session
  exists, so stdout, which carries the response payload, is left clean:

  ```json
  {"claude_version":"2.1.276 (Claude Code)","error_message":"invalid config: malformed.toml: TOML parse error at line 1, column 3\n  |\n1 | [[\n  |   ^\ninvalid key\n","is_error":true,"subtype":"internal_error","type":"result"}
  ```

All three failure tiers share the `error: invalid config:` prefix and exit 2;
they differ in what follows:

- **Unreadable file** — exists but `read` fails (permissions, or the path is a
  directory): `error: invalid config: cannot read config at <path>: Permission
  denied (os error 13)`
- **Parse failure** — malformed TOML, unknown key, or wrong type: reports the
  TOML line and column plus a source excerpt — `error: invalid config: <path>:
  TOML parse error at line 1, column 3` …
- **Constraint violation** — parses, then fails a check above: reports the
  reason instead of a position — `error: invalid config: config validation
  failed at <path>: invalid config: model name 'gpt-4' must start with 'claude-'`

For a quick check without touching the default config:

```bash
bad_config="$(mktemp)"
printf '[[\n' > "$bad_config"
claude-print --config "$bad_config" --output-format json "test prompt" 2>config-error.json
status=$? # 2
jq . config-error.json
rm "$bad_config" config-error.json
```

## Output formats

- `text` (default): plain text response, printed to stdout.
- `json`: one-line JSON object with `type`, `subtype`, `is_error`, `result`, `session_id`, `num_turns`, `duration_ms`, `cost_usd`, `claude_version`, and `usage` fields. `result` holds the response text (there is no `text` or `model` field); `usage` is an object with `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, and `cache_read_input_tokens`.
- `stream-json`: JSONL replay of the raw transcript events in real time, one event per line.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | Assistant error (`is_error: true` in transcript) |
| `2` | Internal error (PTY spawn, hook setup, parse failure) |
| `4` | Input error (no prompt provided, or `--input-file`/stdin unreadable) |
| `124` | Timeout exceeded |
| `130` | Interrupted (SIGINT) |

## How it works

1. **PTY fork** — spawns `claude` under a PTY so `isatty` returns true and the session is tagged `cc_entrypoint=cli`.
2. **Trust dialog dismiss** — watches for the one-time "do you trust this project?" prompt and sends the confirmation keypress automatically.
3. **Bracketed paste injection** — sends the prompt wrapped in bracketed-paste escape sequences (`\x1b[200~` / `\x1b[201~`), which Claude Code's TUI accepts as user input without triggering shell interpretation.
4. **Stop hook FIFO** — installs a temporary Claude Code Stop hook that writes a payload to a FIFO when the response is complete; the process blocks on the FIFO read.
5. **Transcript read** — reads the JSONL session transcript Claude writes to `~/.claude/projects/`, extracts the assistant turn, and emits it in the requested format.

## Warm PTY pool (ADR-005)

`claude-print` can amortize per-invocation startup cost across a fleet by keeping pre-warmed `claude` PTY processes in a pool daemon. This is **opt-in and additive**: without the two pieces below, nothing changes about the ordinary invocation path — no pool code runs at all.

Pooled workers are warmed through trust-dismiss and idle-settle but never past prompt injection, and every request still gets its own `claude` process and session: one prompt per worker, no multi-turn reuse. A released worker is destroyed and replaced, never handed to a second prompt (INV-9).

### `serve` — the pool daemon

```
claude-print serve [--pool-size N] [--socket PATH] [--verbose]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--pool-size <N>` | `1` | Number of workers to keep warm. `0` or anything above `256` exits 2 with an actionable message before any process is spawned |
| `--socket <PATH>` | `/tmp/claude-print-pool.sock` | Unix socket to listen on. Replaces a stale node at the same path; created with owner-only (0600) permissions regardless of umask |
| `--verbose` | off | Daemon lifecycle logging to stderr (spawn / warmup / assign / release / teardown) |

Operating behavior:

- The daemon warms `--pool-size` workers and tops the pool back up after every handout. Warmup is bounded (120 s per worker); a worker that fails or times out warmup is destroyed and replaced, never handed out half-warmed.
- `SIGINT` or `SIGTERM` stops the daemon cleanly: every worker's process group is torn down (SIGTERM → 2 s grace → SIGKILL, exit observed and reaped — no survivors, no zombies), the socket file is removed, and the exit code is 0. A supervisor stopping the service is not a failure. Setup failures (bad `--pool-size`, unusable socket path) exit 2 before any worker is spawned.
- Socket cleanup is ownership-checked: at shutdown the daemon removes only the socket node it created at bind time, so a daemon that lost its path to a replacement never unlinks the winner's socket.
- The worker runs `claude` with the daemon's launch decisions (hook settings for the Stop-FIFO relay, `--setting-sources=` so user hooks do not fire inside pooled workers). Per-invocation client flags cannot reach an already-running worker — see below.
- `serve` never reads the config file and never falls through to prompt validation. Like every entry point it requires a valid `HOME` and a resolvable `claude` binary (`--claude-binary`).

### `--pool-socket` — the client flag

```bash
claude-print --pool-socket /tmp/claude-print-pool.sock "prompt"
```

All three output formats (`text`, `json`, `stream-json`) work over an acquired worker. The acquire is budgeted at `min(60 s, --timeout)`, so the flag can never stall an invocation past its own wall-clock budget. Per-invocation child-launch flags (`--model`, `--max-turns`, the tool-permission flags, `--no-inherit-hooks`, `--mcp-config`, `--pretrust-cwd`) cannot apply to an already-running pooled worker: with `--verbose`, each ignored flag is listed on a stderr diagnostic and the prompt still runs on the daemon's launch.

### Failure behavior and stale sockets

| Pool state | Client behavior |
|---|---|
| Socket file absent, stale (exists, nothing listening), connect refused, or permission denied | Stateless fallback: one `--verbose` diagnostic, then the ordinary session. Output and exit code identical to a no-flag run |
| Daemon answers `pool_full`, `shutting_down`, `internal_error`, or `acquire_timeout` | Stateless fallback, same as above — the pool is up but has nothing to hand out |
| Daemon answers garbage, a malformed frame, an incomplete assignment, or the fd transfer fails; daemon goes silent past the acquire budget | Hard error, exit 2, **no fallback** — a reachable-but-broken daemon must not be masked behind full-price stateless sessions |
| Daemon dies while a client is driving | The session is daemon-independent once assigned: the client finishes inside its `--timeout`; the best-effort release against the dead daemon is bounded (10 s) and non-fatal |
| Client killed without cleanup (SIGKILL) | Its worker is never reassigned to a second prompt; the daemon holds it `InUse` and reclaims it only at its own shutdown |

### Operating limits

- `--pool-size`: 1–256 (hard cap — each worker is a full `claude` PTY process).
- Acquire budget: `min(60 s, --timeout)`, bounding the whole connect + request + response + fd-transfer exchange; every protocol stage inside it is deadline-bounded.
- Worker warmup: bounded at 120 s per worker.
- Release exchange: bounded at 10 s, best-effort.
- One client per worker at a time; concurrent acquirers get distinct workers with distinct sessions.

### Measured startup overhead

Startup overhead (process start → prompt injection, the plan's Benchmark Contract) was measured with the `mock-claude` fixture backend: **1443.6 ms stateless vs 1050.6 ms pooled on average (Δ 393.0 ms, 27.2%)**, release build, 2026-09-19 — see [`docs/notes/startup-overhead-benchmark.md`](docs/notes/startup-overhead-benchmark.md) for method and phase decomposition, and reproduce with `scripts/bench_startup_overhead.py` (build first with `cargo build`; the harness locates the binaries through `cargo metadata` — the same resolution as above, no `--bin-dir` needed on wrapper or stock hosts alike — and `--profile release` benchmarks the release build). This measures `claude-print`'s own overhead only. It does **not** measure model latency: what a real dispatch saves in wall-clock depends on real Claude Code startup and inference, which the mock deliberately removes — no model-latency savings are claimed or established here.

### Billing

Pooled sessions bill exactly like stateless ones — the worker is still `claude` under a PTY, so `cc_entrypoint=cli` holds on both paths (INV-15). Verify the pool path with the credential-backed canary's pooled leg:

```bash
CLAUDE_PRINT_POOL=1 ./scripts/billing-canary.sh
```

### Rollback

The pool is opt-in, so rollback is removing the opt-in — no binary revert required:

1. Point clients away from the pool: remove `--pool-socket <path>` from the invoking config. For NEEDLE that is the `invoke` template in `~/.needle/agents/claude-print.yaml` (which does not set the flag by default).
2. Stop the daemon: send `SIGINT`/`SIGTERM` to the `claude-print serve` process. It exits 0, tears down every worker, and removes its socket file.
3. Removing a stale socket file by hand (`rm /tmp/claude-print-pool.sock`) is always safe — a client that finds a socket nobody is listening on falls back statelessly anyway.

Even out of order (clients still pointing at a dead daemon), behavior stays correct: those clients fall back statelessly (INV-10). A binary-level rollback follows the general release procedure — `install.sh` preserves the previous binary as `claude-print.prev`.

The full invariant set (INV-9 through INV-15) is specified in `docs/plan/plan.md` (Invariants); the end-to-end pins live in `tests/pool_socket_e2e.rs`, `tests/pool_adversarial_e2e.rs`, `tests/pool_failure_e2e.rs`, and `tests/serve.rs`.

## NEEDLE integration

If you use NEEDLE for LLM fleet dispatch, `install.sh` automatically copies `claude-print.yaml` to `~/.needle/agents/`. This registers `claude-print` as the adapter for Anthropic subscription models (sonnet/opus/haiku) so NEEDLE workers bill against the subscription rather than the Agent SDK credit pool. See `claude-print.yaml` in the repo root for the full adapter config, including `--no-inherit-hooks` isolation mode and the `use_or_lose` cost type.

## Limitations

- **Linux only** — PTY allocation is POSIX. No Windows ConPTY support.
- **Claude Code must be authenticated** — `claude-print` delegates entirely to the `claude` binary; it cannot authenticate on its own.
- **One prompt per invocation** — there is no multi-turn session mode; each call starts a fresh session.
- **Startup latency ~2–5s** — the PTY handshake and Claude Code startup add overhead versus a direct HTTP call.
- **Concurrent `stream-json` invocations sharing a cwd** — safe for every configuration that writes the per-drive `session-identity.json` (real `claude` on both the stateless and `--pool-socket` paths, and `mock-claude`): the stream-json reader binds to this session's exact transcript at prompt-submission time, before any assistant event exists, so a sibling session's events are never forwarded. The residual limit is identity-less sessions — a `claude` without UserPromptSubmit hook support: under same-cwd concurrency such a run binds only an unambiguous single new transcript, and otherwise forwards nothing live until the Stop payload names the transcript at the end (output arrives whole and uncontaminated, but not streamed). `text` and `json` are exact in every configuration.

## Troubleshooting

### HOME in containers and chroots

Running a session, `--check`, or `--version` requires `HOME` to contain the
current user's home directory. `claude-print` deliberately does not guess
`/root` or consult the passwd database: choosing the wrong directory could read
or write another user's Claude Code configuration and transcripts.

If `HOME` is unset or empty, text output exits with status 2 and reports:

```text
error: invalid config: HOME environment variable not set or empty; set HOME to the user's home directory
```

The `json` and `stream-json` output formats also exit with status 2 and put the
same message in the result object's `error_message` field. Setting
`XDG_CONFIG_HOME` does not remove the `HOME` requirement: it can relocate
`claude-print`'s config file, but Claude Code state and transcripts still live
under `$HOME`.

In a container, chroot, or service unit, set `HOME` explicitly to the home of
the account that runs `claude-print`. Create that directory with the correct
ownership and mount or provision the user's authenticated Claude Code state
there. For example:

```dockerfile
ENV HOME=/home/claude
```

```yaml
# Kubernetes container specification
env:
  - name: HOME
    value: /home/claude
```

For a chroot or one-off service invocation, the equivalent is
`HOME=/home/service claude-print "..."`. Do not use `/root` unless the process
actually runs as root and `/root` is intentionally where its Claude Code state
is stored. Startup verifies that the configured path exists, is a directory,
and permits a temporary file to be created and written. This detects missing
home mounts, permission problems, and read-only filesystems before Claude Code
starts. The probe file is removed immediately; failures name the configured
path and never fall back to `/root`.

**Migration note:** Older builds checked `HOME` only while resolving particular
paths, accepted an empty or unprovisioned value at some call sites, and could
run early-exit commands such as `--version` without it. Current builds enforce
one consistent, existing-and-writable `HOME` contract before session startup,
`--check`, and `--version`. When upgrading existing containers or service
definitions, add an explicit `HOME`, create or mount it with write permission,
and update health checks that invoke `--version` in a stripped environment.
Argument-parser help (`--help`) is still rendered before runtime HOME validation.

### Billing classification verification

Three names are involved here and they are not the same thing — see
[`docs/notes/billing-context.md`](docs/notes/billing-context.md) for the full
contract:

- `cc_entrypoint` — Anthropic's wire-level billing header, chosen by Claude
  Code at startup. Never an environment variable, never directly observable.
- `CLAUDE_CODE_ENTRYPOINT` — the environment input claude-print controls:
  forced to `cli` in the child regardless of inheritance (`FORCED_ENV`,
  `src/pty.rs`), verified credential-free by `claude-print --check`.
- `entrypoint` (transcript JSONL field) — the observable evidence of the
  classification Claude Code actually chose. This is what the checks below
  assert on.

Before deploying to production, verify that sessions are billing against the subscription pool (`cc_entrypoint=cli`):

```bash
# Check the most recent session's billing classification
./scripts/check-billing.sh
```

This script inspects the latest transcript JSONL under `~/.claude/projects/` and asserts the `entrypoint` field is `"cli"` (subscription), not `"sdk-cli"` (credit pool). Exit 0 means correct billing; exit 1 means a billing regression. Run this after every release or Claude Code upgrade.

Production hosts also run a daily credential-backed canary. Install its
systemd user timer on ex44 and lab with:

```bash
./scripts/install-billing-canary.sh
```

The canary makes a single one-turn Haiku invocation, checks that invocation's exact
transcript with `check-billing.sh`, and atomically writes `PASS` or `FAIL` to
`~/.local/state/claude-print/billing-canary/last-result`. See
[`scripts/billing-canary.md`](scripts/billing-canary.md) for timer and alerting
details. The manual release check remains required as a second layer.

### Common issues

**PTY open failed** — You may be in a container without `/dev/ptmx`. Run on a bare-metal host or a VM with full PTY support.

**Session never completes** — The Stop hook may not be firing. Check `--verbose` output for "Stop received" and verify your `~/.claude/settings.json` isn't blocking hook execution.

**Empty output despite success** — The transcript reader may have hit a race condition. Run with `--verbose` to see retry attempts; if retries exceed 40×50ms, the Stop hook fired before the JSONL was flushed.

## Release checklist

Before cutting a release tag:

1. Run `./scripts/check-billing.sh` to verify billing conformance (requires credentials)
2. Run `cargo test` to ensure all mocked tests pass
3. Run `cargo run --bin claude-print -- --check` to verify PTY, FIFO, and the billing env-input force (credential-free) on the build you are about to release
4. **Check Claude Code version currency**: if the installed Claude Code version (`claude --version`) has changed since the last release, capture a real session transcript and add it as `tests/fixtures/transcript_vX.Y.Z.jsonl` with corresponding regression tests in `tests/version_compat.rs`
5. Update version in `Cargo.toml`
6. Commit and push: `git tag v0.x.y && git push origin v0.x.y` (origin is Forgejo, the canonical host — the tag must land there first; see the workflow's tag-to-Forgejo note)
7. Monitor the `claude-print-ci` Argo Workflow for a successful build and publish — release artifacts land on GitHub Releases, the supported download channel (see [Repository & contributions](#repository--contributions))

## Structure

- `docs/notes/` — design decisions, constraints, integration details
- `docs/plan/plan.md` — complete implementation plan
- `scripts/check-billing.sh` — AS-4 billing conformance script (run before every release)
- `scripts/billing-canary.sh` — daily credential-backed AS-4 canary (`CLAUDE_PRINT_POOL=1` runs the pooled leg)
- `scripts/claude-print-billing-canary.{service,timer}` — systemd user units for the canary
- `scripts/bench_startup_overhead.py` — ADR-005 startup-overhead benchmark harness (`--self-check` for a deterministic no-subprocess pin)
- `scripts/contract-maintenance-gate.sh` — Claude contract-evidence maintenance gate (detect version drift → re-run probes → evidence bundle → re-pin follow-up; exits 0 = current, 1 = re-run due, 2 = indeterminate; CI runs it on every push, see `docs/notes/claude-contract-probes.md` §Maintenance)
- `scripts/` — integration test scripts

---

Part of [jedarden.com](https://jedarden.com)

*This GitHub repo is a read-only mirror of git.ardenone.com/jedarden/claude-print — issues and PRs are welcome here either way.*
