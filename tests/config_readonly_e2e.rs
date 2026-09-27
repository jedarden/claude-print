//! Binary-level pin of the strictly read-only config load (bead
//! claudepr-39d4178e).
//!
//! `docs/notes/config-file-contract.md` §"Scope" states claude-print "never
//! creates, writes, or scaffolds the file — you own it entirely", and §"Missing
//! file" that a missing config — discovered *or* named by `--config` — is a
//! defined non-error. The contract's own fixture (`tests/config_contract.rs`)
//! replays the loader against files the test itself wrote, and
//! `tests/config_entry_point_scope.rs` pins which entry points read the file;
//! neither held the filesystem side of the read-only guarantee — that a prompt
//! run leaves a missing path missing and an existing file untouched. A
//! regression that "helpfully" scaffolded `$XDG_CONFIG_HOME/claude-print/
//! config.toml`, rewrote the file to normalized form, or reset its permissions
//! would pass every existing suite.
//!
//! These tests run the *compiled* `claude-print` binary through full
//! mock-claude prompt runs (same hermetic strategy as `tests/binary_e2e.rs`
//! and `tests/config_entry_point_scope.rs`), with `HOME`, `TMPDIR`, and the
//! config channel all pointed inside one temp root via child-env overrides —
//! the process environment is never mutated. The pin, per channel (the
//! contract's three path-resolution rules: `$XDG_CONFIG_HOME` discovery,
//! `$HOME/.config` discovery, and an explicit `--config`):
//!
//!   * **No creation** — a prompt run with the channel's config path missing
//!     must succeed and leave the path missing: no `config.toml`, no
//!     `claude-print/` directory, no `$HOME/.config` scaffold at all, and no
//!     parent directory materialized for an explicit `--config` either.
//!   * **No rewrite** — a prompt run with a hand-written config at the
//!     channel's path must succeed, must demonstrably have loaded the file
//!     (the child argv carries the config's `--model`, a value that is neither
//!     the built-in default nor passed on the CLI), and must leave the file
//!     byte-identical: contents, permissions, mtime, inode, and size all
//!     unchanged.
//!
//! Non-vacuity is proven, not assumed, on both sides:
//!   * the preservation runs' recorded child argv proves the config was
//!     genuinely read and applied (not skipped by an early exit);
//!   * `garbage_at_a_wired_channel_is_lethal` places `[[` at each wired
//!     channel and pins the prompt run's exit-2 `invalid config` error — the
//!     same control shape as `tests/config_entry_point_scope.rs`'s
//!     `every_poison_is_lethal_when_read`, here proving THIS suite's env
//!     wiring really directs the config step at the paths the absence and
//!     preservation assertions are made about. Without it, a wiring mistake
//!     (an env var that never reached the child) would make every read-only
//!     assertion pass while pinning nothing.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Per-run wall-clock budget. mock-claude completes a session within ~2s; 30s
/// is a generous ceiling that still fails fast on a wedge (same bound as
/// `tests/binary_e2e.rs`).
const BUDGET: Duration = Duration::from_secs(30);

/// The hand-written config the preservation runs start from. Deliberately
/// non-canonical — a leading comment, an unknown root-level key the loader
/// must silently ignore, the model set — so a rewrite that normalized,
/// re-ordered, or re-serialized the file could not reproduce it byte-for-byte
/// even with the same effective values.
const PRESERVED_CONFIG: &str = "# hand-written config — claude-print must not rewrite me\nextra = \"kept by the root-level ignore rule\"\n[defaults]\nmodel = \"claude-opus-4-8\"\n";

/// The model `PRESERVED_CONFIG` sets: neither the compiled-in default
/// (`claude-sonnet-4-6`) nor a CLI flag in these runs, so its appearance in
/// the recorded child argv can only come from the config file having been
/// loaded and applied.
const PRESERVED_MODEL: &str = "claude-opus-4-8";

/// A captured subprocess outcome: exit code (or `None` if killed on timeout),
/// and decoded stdout/stderr.
#[derive(Debug)]
struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Locate a workspace bin built alongside this test binary. Test binaries
/// live at `target/<profile>/deps/`; named workspace bins live at
/// `target/<profile>/`. `mock-claude` is a bin target of this package, so any
/// `cargo test` run links it into `target/<profile>/` next to the test
/// binaries (claudepr-2c965921).
fn workspace_bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("test binary must live under target/<profile>/deps/");
    profile_dir.join(name)
}

/// Build a `claude-print` Command pre-wired to use mock-claude as the backend.
fn claude_print() -> Command {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    assert!(
        bin.exists(),
        "claude-print binary missing at {}; run `cargo build`",
        bin.display()
    );
    assert!(
        mock.exists(),
        "mock-claude binary missing at {}; run `cargo build`",
        mock.display()
    );
    let mut cmd = Command::new(&bin);
    cmd.arg("--claude-binary").arg(&mock);
    cmd
}

/// Run `cmd` to completion, decoding stdout/stderr as UTF-8. Kills the child
/// if it outlives `budget` so a wedge cannot hang the suite.
fn run(cmd: &mut Command, budget: Duration) -> Outcome {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn claude-print: {e}"));

    let deadline = start + budget;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("claude-print did not exit within {budget:?}");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            // Reaping error: treat as killed.
            Err(_) => break None,
        }
    };

    let output = child
        .wait_with_output()
        .expect("wait_with_output after try wait");
    Outcome {
        code: code.or(output.status.code()),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Read mock-claude's MOCK_RECORD_ARGS dump (NUL-separated argv) into a
/// `Vec<String>`, panicking if the file was never written (same helper as
/// `tests/binary_e2e.rs`).
fn read_recorded_argv(path: &Path) -> Vec<String> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| {
        panic!(
            "MOCK_RECORD_ARGS file was not written at {}: {e}",
            path.display()
        )
    });
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// The contract's three path-resolution rules, as one runnable wiring
/// (`docs/notes/config-file-contract.md` §"File location and path
/// precedence").
enum Channel {
    /// Rule 2: `$XDG_CONFIG_HOME/claude-print/config.toml`.
    DiscoveredXdg,
    /// Rule 3: `$HOME/.config/claude-print/config.toml`, with
    /// `XDG_CONFIG_HOME` unset so discovery goes through `HOME`.
    DiscoveredHome,
    /// Rule 1: an explicit `--config <FILE>` (root-relative), which replaces
    /// discovery entirely.
    Flag(PathBuf),
}

impl Channel {
    fn label(&self) -> String {
        match self {
            Channel::DiscoveredXdg => "the discovered path via $XDG_CONFIG_HOME".to_string(),
            Channel::DiscoveredHome => "the discovered path via $HOME/.config".to_string(),
            Channel::Flag(path) => format!("an explicit --config ({})", path.display()),
        }
    }
}

/// Write `content` at `path` (creating parent directories) with mode 0600 —
/// deliberately distinct from `fs::write`'s 0644 default, so a rewrite that
/// restored default permissions cannot pass the preservation mode pin by
/// accident. `None` writes nothing: the missing-path runs start from absence.
fn write_config(content: Option<&str>, path: &Path) {
    let content = match content {
        Some(content) => content,
        None => return,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("create config parent {}: {e}", parent.display()));
    }
    std::fs::write(path, content)
        .unwrap_or_else(|e| panic!("write config {}: {e}", path.display()));
    let mut perms = std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat config {}: {e}", path.display()))
        .permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
        .unwrap_or_else(|e| panic!("chmod 600 config {}: {e}", path.display()));
}

/// Wire one run's environment inside `root`: `HOME` and `TMPDIR` are private
/// directories (the process-wide HOME gate needs a writable HOME; a private
/// TMPDIR keeps the hook installer's per-run temp dirs out of the shared
/// /tmp), discovery is pointed at the channel's location, `--config` names
/// the `Flag` channel's path, and an existing config is written there when
/// `content` is `Some`. Returns the channel's config path — the path every
/// absence/preservation assertion is made about. `MOCK_RECORD_ARGS` is always
/// wired (into `root`, never into a location an assertion inspects) so any
/// run can prove it loaded the config it found.
fn wire_run(cmd: &mut Command, root: &Path, channel: Channel, content: Option<&str>) -> PathBuf {
    let home = root.join("home");
    let tmp = root.join("tmp");
    std::fs::create_dir_all(&home).expect("create run HOME");
    std::fs::create_dir_all(&tmp).expect("create run TMPDIR");
    cmd.env("HOME", &home).env("TMPDIR", &tmp);
    cmd.env("MOCK_RECORD_ARGS", root.join("child-argv"));

    match channel {
        Channel::DiscoveredXdg => {
            let xdg = root.join("xdg");
            std::fs::create_dir_all(&xdg).expect("create XDG dir");
            cmd.env("XDG_CONFIG_HOME", &xdg);
            let path = xdg.join("claude-print").join("config.toml");
            write_config(content, &path);
            path
        }
        Channel::DiscoveredHome => {
            // XDG unset in the child: discovery goes through HOME's
            // `.config`, and HOME itself must stay a valid writable directory
            // for the process-wide HOME gate.
            cmd.env_remove("XDG_CONFIG_HOME");
            let path = home
                .join(".config")
                .join("claude-print")
                .join("config.toml");
            write_config(content, &path);
            path
        }
        Channel::Flag(rel) => {
            // Discovery stays pointed at an empty dir so the flag is the only
            // config channel in the run (--config replaces discovery).
            let xdg = root.join("xdg-clean");
            std::fs::create_dir_all(&xdg).expect("create clean XDG dir");
            cmd.env("XDG_CONFIG_HOME", &xdg);
            let path = root.join(rel);
            write_config(content, &path);
            cmd.arg("--config").arg(&path);
            path
        }
    }
}

/// Assert a prompt run completed a healthy session: exit 0 and text on stdout
/// (AS-1 shape) — proving the run passed the HOME gate AND the config-load
/// step AND drove mock-claude through a full session, not an early exit that
/// would make the filesystem assertions vacuous.
fn assert_healthy_prompt_run(out: &Outcome, context: &str) {
    assert_eq!(
        out.code,
        Some(0),
        "{context}: expected a successful prompt run\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        !out.stdout.trim().is_empty(),
        "{context}: a healthy text-mode session writes stdout, got:\n{}",
        out.stdout
    );
}

/// The full observable state of a config file. `atime` is deliberately
/// absent: reads may legitimately update it (relatime), and reading is exactly
/// what a prompt run is supposed to do. Every field here changes only when the
/// file is written, replaced, or re-chmodded — the acts the read-only contract
/// forbids: a rewrite-in-place moves the mtime, a replace-through-rename
/// changes the inode, and a permissions "fix" changes the mode.
struct Snapshot {
    bytes: Vec<u8>,
    mode: u32,
    mtime: (i64, i64),
    inode: u64,
    size: u64,
}

fn snapshot(path: &Path) -> Snapshot {
    let bytes =
        std::fs::read(path).unwrap_or_else(|e| panic!("read config {}: {e}", path.display()));
    let md =
        std::fs::metadata(path).unwrap_or_else(|e| panic!("stat config {}: {e}", path.display()));
    Snapshot {
        bytes,
        mode: md.mode() & 0o7777,
        mtime: (md.mtime(), md.mtime_nsec()),
        inode: md.ino(),
        size: md.len(),
    }
}

// ── No creation: missing paths stay missing ──────────────────────────────────

/// A prompt run with no config at the `$XDG_CONFIG_HOME` discovered path
/// succeeds on built-in defaults and creates nothing on the way: no
/// `config.toml`, and no `claude-print/` directory scaffold beneath the XDG
/// root either — "never creates, writes, or scaffolds" forbids every depth.
#[test]
fn prompt_run_never_creates_the_missing_xdg_discovered_config() {
    let shared = tempfile::tempdir().expect("test temp dir");
    let root = shared.path();
    let mut cmd = claude_print();
    let config_path = wire_run(&mut cmd, root, Channel::DiscoveredXdg, None);
    cmd.arg("test prompt");
    let out = run(&mut cmd, BUDGET);
    assert_healthy_prompt_run(&out, "missing XDG-discovered config");

    let context = "a prompt run must not scaffold the XDG config location";
    assert!(
        !config_path.exists(),
        "{context}: {} was created",
        config_path.display()
    );
    let claude_print_dir = config_path
        .parent()
        .expect("discovered path has a claude-print/ parent");
    assert!(
        !claude_print_dir.exists(),
        "{context}: the directory {} was scaffolded",
        claude_print_dir.display()
    );
    assert!(
        root.join("xdg").exists(),
        "{context}: the XDG root itself must survive the run untouched"
    );
}

/// A prompt run with `XDG_CONFIG_HOME` unset and no config under
/// `$HOME/.config` creates nothing there. The assertion is deliberately the
/// strong form — `.config` itself must not appear, not just the file or the
/// `claude-print/` subdirectory: nothing else in a prompt run writes under
/// `$HOME/.config` (Claude Code state and transcripts live under
/// `$HOME/.claude`, the HOME-gate probe writes and removes a temp file in
/// `$HOME` itself), so any appearance of `.config` is config scaffolding.
#[test]
fn prompt_run_never_creates_the_missing_home_discovered_config() {
    let shared = tempfile::tempdir().expect("test temp dir");
    let root = shared.path();
    let home = root.join("home");
    let mut cmd = claude_print();
    let config_path = wire_run(&mut cmd, root, Channel::DiscoveredHome, None);
    cmd.arg("test prompt");
    let out = run(&mut cmd, BUDGET);
    assert_healthy_prompt_run(&out, "missing HOME-discovered config");

    let context = "a prompt run must not scaffold $HOME/.config";
    assert!(
        !config_path.exists(),
        "{context}: {} was created",
        config_path.display()
    );
    assert!(
        !home.join(".config").exists(),
        "{context}: $HOME/.config itself was scaffolded — no other prompt-run \
         write goes there, so its appearance is config scaffolding"
    );
}

/// An explicit `--config <FILE>` that does not exist is a defined non-error
/// (§"Missing file"), and the run must not "fix" the situation: the named
/// file stays absent when its parent directory exists, and a path whose
/// parent directory does not exist leaves that directory uncreated too — no
/// scaffold through the flag channel either.
#[test]
fn prompt_run_never_creates_a_missing_config_named_by_flag() {
    // Flat: parent exists, file missing.
    let shared = tempfile::tempdir().expect("flat temp dir");
    let root = shared.path();
    let mut cmd = claude_print();
    let config_path = wire_run(
        &mut cmd,
        root,
        Channel::Flag(PathBuf::from("flag-config.toml")),
        None,
    );
    cmd.arg("test prompt");
    let out = run(&mut cmd, BUDGET);
    assert_healthy_prompt_run(&out, "missing --config file");
    assert!(
        !config_path.exists(),
        "a prompt run must not create the --config-named file {}: it was \
         materialized by the run",
        config_path.display()
    );

    // Nested: the parent directory itself does not exist.
    let nested = tempfile::tempdir().expect("nested temp dir");
    let root = nested.path();
    let mut cmd = claude_print();
    let config_path = wire_run(
        &mut cmd,
        root,
        Channel::Flag(PathBuf::from("absent-dir").join("config.toml")),
        None,
    );
    cmd.arg("test prompt");
    let out = run(&mut cmd, BUDGET);
    assert_healthy_prompt_run(&out, "missing --config file in a missing directory");
    assert!(
        !config_path.exists(),
        "a prompt run must not create the --config-named file {}",
        config_path.display()
    );
    assert!(
        !root.join("absent-dir").exists(),
        "a prompt run must not scaffold the --config-named file's parent \
         directory — creating absent-dir/ is scaffolding, not loading"
    );
}

// ── No rewrite: existing configs stay byte-identical ─────────────────────────

/// A prompt run that loads a hand-written config leaves it untouched, on every
/// path-resolution rule. Non-vacuity is structural: the recorded child argv
/// carries `--model claude-opus-4-8` — the config's own value, which is
/// neither the built-in default nor passed on the CLI in these runs — so the
/// file was demonstrably read and applied, and the snapshot comparison then
/// pins that reading was ALL the run did to it: contents, permissions, mtime,
/// inode, and size are all unchanged (an in-place rewrite moves the mtime; a
/// replace-through-rename changes the inode; a permissions "fix" changes the
/// mode — each field catches a different rewrite strategy).
#[test]
fn prompt_run_preserves_an_existing_config_on_every_channel() {
    for channel in [
        Channel::DiscoveredXdg,
        Channel::DiscoveredHome,
        Channel::Flag(PathBuf::from("flag-config.toml")),
    ] {
        let context = format!(
            "a prompt run loading an existing config from {}",
            channel.label()
        );
        let shared = tempfile::tempdir().expect("preservation temp dir");
        let root = shared.path();
        let mut cmd = claude_print();
        let config_path = wire_run(&mut cmd, root, channel, Some(PRESERVED_CONFIG));
        cmd.arg("test prompt");
        let before = snapshot(&config_path);
        let out = run(&mut cmd, BUDGET);
        assert_healthy_prompt_run(&out, &context);

        // Loaded-and-applied proof: the config's model reached the child argv.
        let args = read_recorded_argv(&root.join("child-argv"));
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--model" && w[1] == PRESERVED_MODEL),
            "{context}: the run must have loaded {} and forwarded its model \
             (non-vacuity — an early exit would leave the file untouched for \
             free), recorded argv: {args:?}",
            config_path.display()
        );

        let after = snapshot(&config_path);
        assert_eq!(
            after.bytes, before.bytes,
            "{context}: contents were rewritten — the read-only contract \
             forbids writes, and the hand-written formatting (comment, ignored \
             root key) cannot survive a re-serialization"
        );
        assert_eq!(
            after.mode, before.mode,
            "{context}: permissions changed (was pinned to 0600 before the run)"
        );
        assert_eq!(
            after.mtime, before.mtime,
            "{context}: mtime moved — the file was written during the run"
        );
        assert_eq!(
            after.inode, before.inode,
            "{context}: inode changed — the file was replaced (rename-style \
             rewrite), not left alone"
        );
        assert_eq!(
            after.size, before.size,
            "{context}: size changed — the file was rewritten"
        );
    }
}

// ── Non-vacuity control: the wiring really reaches the config step ───────────

/// The other half of the pin. Every channel's wiring is proven live by
/// placing garbage TOML (`[[`, the parse-tier poison) at the wired path and
/// requiring the prompt run's hard exit-2 `invalid config` error — the exact
/// control shape `tests/config_entry_point_scope.rs` applies to its own
/// matrix. This is what makes the absence assertions above meaningful: if the
/// env wiring failed to reach the child (a typo'd variable, a wrong path),
/// the config step would be resolving some other location, the missing paths
/// would trivially stay missing, and these tests would pass while pinning
/// nothing.
#[test]
fn garbage_at_a_wired_channel_is_lethal() {
    for channel in [
        Channel::DiscoveredXdg,
        Channel::DiscoveredHome,
        Channel::Flag(PathBuf::from("flag-config.toml")),
    ] {
        let context = format!("prompt path with garbage TOML at {}", channel.label());
        let shared = tempfile::tempdir().expect("control temp dir");
        let mut cmd = claude_print();
        wire_run(&mut cmd, shared.path(), channel, Some("[[\n"));
        cmd.arg("test prompt");
        let out = run(&mut cmd, BUDGET);

        assert_eq!(
            out.code,
            Some(2),
            "{context}: reading a poisoned config must be a hard exit-2 error — \
             this control proves the suite's wiring directs the config step at \
             the asserted paths\nstdout:\n{}\nstderr:\n{}",
            out.stdout,
            out.stderr
        );
        assert!(
            out.stderr.contains("invalid config"),
            "{context}: stderr must carry the parse-tier message, got:\n{}",
            out.stderr
        );
    }
}
