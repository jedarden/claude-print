//! `--pretrust-cwd` trust pre-grant contract (bead claudepr-bd36930d).
//!
//! `docs/notes/pretrust-cwd-contract.md` is the normative definition behind the
//! compat contract's one-line summary ("never (writes `~/.claude.json` before
//! spawn)"). This suite holds that document against the implementation in both
//! directions:
//!
//!   * **trigger / timing / pool exclusion** — the doc's §1–§2 claims pinned to
//!     the real source: the flag is a `Cli` field, the pretrust gate sits in
//!     `Session::run`'s launch path ordered after version resolution and before
//!     child-argv construction, the pooled launch region contains no pretrust
//!     call at all, and main's pooled inapplicability list names the flag;
//!   * **schema + write mechanics** — observed end-to-end through the compiled
//!     binary: a fresh `HOME` gains exactly `projects[<cwd>].
//!     hasTrustDialogAccepted: true` at mode `0600`; a populated trust file
//!     keeps every sibling project, unrelated root key, and unrelated entry
//!     field while gaining the grant; the merge canonicalizes to one sorted
//!     compact line; a second run is byte-identical; a pre-existing mode
//!     survives;
//!   * **failure taxonomy** — each soft row (unparseable, non-object root,
//!     permission-denied) leaves the file byte-identical behind a
//!     `claude-print: warning:` line with the run still succeeding through the
//!     scanner-dismissed dialog and no temporary file left behind; each hard
//!     row (conflicting `projects` shapes) exits 2 without writing and without
//!     ever spawning a child, in both the text and the json rendering;
//!   * **child side** — the recorded child argv never contains the flag while
//!     the same run demonstrably wrote the grant, and the trusted/untrusted A/B
//!     through mock-claude's `MOCK_TRUST_FROM_CLAUDE_JSON` trust read: an
//!     untrusted cwd renders the dialog (the unresolvable-wording control
//!     exits 2) while the `--pretrust-cwd` run of the same directory suppresses
//!     it — non-vacuous proof the pre-grant reached the child in the exact
//!     schema, before spawn.
//!
//! Hermetic: compiled `claude-print` + mock-claude; every run gets a throwaway
//! `HOME`, an explicit empty `--config`, and its own working directory, so the
//! trust grant, the transcripts, and the cwd key all land in temp dirs that die
//! with the test. Child-env overrides only — this process never mutates its own
//! environment.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

// ── repo / doc plumbing ──────────────────────────────────────────────────────

/// Repo root, resolved the same way as `tests/docs_test_classification.rs`:
/// an explicit override first (harnesses that copy the tree), then the runtime
/// manifest dir cargo sets for this test build.
fn repo_root() -> PathBuf {
    if let Ok(dir) = std::env::var("CLAUDE_PRINT_TEST_REPO") {
        let dir = PathBuf::from(dir);
        if dir.join("Cargo.toml").exists() {
            return dir;
        }
    }
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

fn contract_doc() -> String {
    let path = repo_root().join("docs/notes/pretrust-cwd-contract.md");
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("contract doc missing at {}: {e}", path.display()))
}

fn src_source(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("source missing at {}: {e}", path.display()))
}

// ── binary-e2e plumbing (the `tests/claude_p_compat_contract.rs` pattern) ───

/// Locate a workspace bin built alongside this test binary (`current_exe()` →
/// `target/<profile>/deps/` → `target/<profile>/`), so the suite never names a
/// written-out build path.
fn workspace_bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    exe.parent()
        .and_then(|p| p.parent())
        .expect("test binary must live under target/<profile>/deps/")
        .join(name)
}

/// Per-session wall-clock budget: mock-claude answers within a couple of
/// seconds; 30 s is a generous ceiling that still fails fast on a wedge.
const BUDGET: Duration = Duration::from_secs(30);

/// One test's hermetic world: a throwaway `HOME` (the trust file, the
/// transcripts, and the config probe all land here), an explicit empty
/// `--config`, and a separate working directory whose resolved path is the
/// trust grant's key. Keep the `TempDir`s alive for the run.
struct World {
    /// Throwaway home — `$HOME/.claude.json` inside it is the contract's file.
    home: TempDir,
    /// The directory the session runs in; its resolved path is the grant key.
    workdir: TempDir,
    /// The explicit `--config` path (an empty file).
    config: PathBuf,
}

impl World {
    fn new() -> Self {
        let home_dir = TempDir::new().expect("temp home");
        let workdir = TempDir::new().expect("temp workdir");
        let config = home_dir.path().join("config.toml");
        fs::write(&config, "").expect("write empty config");
        Self {
            home: home_dir,
            workdir,
            config,
        }
    }

    fn home_path(&self) -> &Path {
        self.home.path()
    }

    /// The trust file's path inside this world.
    fn claude_json(&self) -> PathBuf {
        self.home.path().join(".claude.json")
    }

    /// The trust grant's key: the cwd both claude-print (`getcwd`) and the
    /// child observe, symlink-resolved the way `getcwd(3)` reports it.
    fn cwd_key(&self) -> String {
        fs::canonicalize(self.workdir.path())
            .expect("canonicalize workdir")
            .to_string_lossy()
            .into_owned()
    }

    /// A `claude-print` command for this world, running in its workdir with the
    /// hermetic HOME + config already wired. Extra knobs (mock seams) come
    /// from the caller via `env`.
    fn cmd(&self) -> Command {
        let mut cmd = Command::new(workspace_bin("claude-print"));
        cmd.current_dir(self.workdir.path());
        cmd.env("HOME", self.home.path());
        cmd.env("MOCK_UNIQUE_SESSION_ID", "1");
        cmd
    }

    /// The full invocation for this world: `--claude-binary <mock>` and the
    /// explicit `--config`, ahead of whatever else the caller appends.
    fn args(&self, cmd: &mut Command) {
        cmd.arg("--claude-binary").arg(workspace_bin("mock-claude"));
        cmd.arg("--config").arg(&self.config);
    }
}

struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run to completion with an explicit stdin; kill (and fail) past `budget`.
fn run(cmd: &mut Command, budget: Duration, stdin: Stdio) -> Outcome {
    cmd.stdin(stdin)
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
            Err(_) => break None,
        }
    };
    let output = child.wait_with_output().expect("wait_with_output");
    Outcome {
        code: code.or(output.status.code()),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Read mock-claude's `MOCK_RECORD_ARGS` dump (NUL-separated argv).
fn read_recorded_argv(path: &Path) -> Vec<String> {
    let bytes = fs::read(path).unwrap_or_else(|e| {
        panic!(
            "MOCK_RECORD_ARGS file was not written at {}: {e}",
            path.display()
        )
    });
    bytes
        .split(|b| *b == 0)
        .filter(|e| !e.is_empty())
        .map(|e| String::from_utf8_lossy(e).into_owned())
        .collect()
}

/// The trust grant's JSON value in a world's trust file, panics with the file's
/// content if the file is missing or unparseable.
fn trust_file_value(world: &World) -> serde_json::Value {
    let content = fs::read_to_string(world.claude_json()).unwrap_or_else(|e| {
        panic!(
            "trust file missing at {}: {e}",
            world.claude_json().display()
        )
    });
    serde_json::from_str(&content).expect("trust file must be a valid JSON document")
}

/// No pretrust temporary file may survive a run, whatever the outcome.
fn assert_no_tmp_leftover(world: &World) {
    let leftovers: Vec<PathBuf> = fs::read_dir(world.home_path())
        .expect("read HOME")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .map(|n| {
                    n.to_string_lossy()
                        .starts_with(".claude.json.tmp-claude-print-")
                })
                .unwrap_or(false)
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "no pretrust temporary file may survive the run: {leftovers:?}"
    );
}

/// The knob + wording pair every dialog-observation run uses. With
/// `MOCK_TRUST_FROM_CLAUDE_JSON` the mock reads the same trust file real claude
/// does; with `unresolvable` wording a rendered dialog forces the driver's
/// exit-2 refusal — so a successful run proves the dialog never rendered.
fn observe_dialog(cmd: &mut Command) {
    cmd.env("MOCK_TRUST_FROM_CLAUDE_JSON", "1");
    cmd.env("MOCK_TRUST_WORDING", "unresolvable");
}

// ── §1–§2: trigger, timing, and pool exclusion against the source ───────────

/// The doc's trigger/timing/pool claims, pinned to the real source: the flag is
/// a parser field wired into `LaunchOptions`; the pretrust gate sits inside
/// `Session::run`'s launch path ordered after version resolution and before
/// child-argv construction and spawn; the pooled launch region contains no
/// pretrust call at all; and main's pooled inapplicability list names the flag.
/// A handful of load-bearing doc strings are held against the constants the
/// implementation actually writes (schema key, tmp-file name, default mode,
/// warning shapes) so the document cannot drift from them silently.
#[test]
fn doc_trigger_timing_and_pool_exclusion_match_the_source() {
    let doc = contract_doc();
    let cli_src = src_source("src/cli.rs");
    let main_src = src_source("src/main.rs");
    let session_src = src_source("src/session.rs");

    // §1: the flag exists and is wired into the launch options.
    assert!(
        cli_src.contains("long = \"pretrust-cwd\"") && cli_src.contains("pub pretrust_cwd: bool"),
        "src/cli.rs must define the --pretrust-cwd flag"
    );
    assert!(
        main_src.contains("pretrust_cwd: cli.pretrust_cwd"),
        "src/main.rs must wire the flag into LaunchOptions"
    );

    // §2: inside the stateless launch path the gate is ordered
    // version resolution → pretrust → child argv build → spawn.
    let version_at = session_src
        .find("Self::resolve_claude_version(claude_bin)?")
        .expect("run_inner must resolve the claude version");
    let gate_at = session_src
        .find("if launch.pretrust_cwd {")
        .expect("run_inner must gate the pretrust write on the flag");
    let pretrust_at = session_src
        .find("pretrust_cwd()?")
        .expect("run_inner must call the pretrust write");
    let argv_at = session_src
        .find("Self::build_child_argv(claude_bin, &installer, launch, claude_args)?")
        .expect("run_inner must build the child argv");
    let spawn_at = session_src
        .find("PtySpawner::spawn(&cmd, &args)?")
        .expect("run_inner must spawn the PTY child");
    assert!(
        version_at < gate_at
            && gate_at < pretrust_at
            && pretrust_at < argv_at
            && argv_at < spawn_at,
        "the doc's §2 ordering (version resolution → pretrust → argv → spawn) \
         must hold in src/session.rs's launch path"
    );

    // §1: the pooled launch region runs no pretrust write at all.
    let pooled_start = session_src
        .find("pub fn run_pooled(")
        .expect("run_pooled must exist");
    let pooled_end = session_src[pooled_start..]
        .find("fn resolve_claude_version(")
        .expect("resolve_claude_version definition must follow the pooled region")
        + pooled_start;
    let pooled_region = &session_src[pooled_start..pooled_end];
    assert!(
        !pooled_region.contains("pretrust"),
        "the pooled launch path must never run the pretrust write (§1: reported \
         inapplicable, never applied)"
    );
    assert!(
        main_src.contains("inapplicable.push(\"--pretrust-cwd\")"),
        "main.rs's pooled inapplicability diagnostic must name --pretrust-cwd"
    );

    // The doc names the constants the implementation actually writes.
    assert!(
        session_src.contains("hasTrustDialogAccepted"),
        "the implementation must write the documented schema key"
    );
    assert!(
        session_src.contains(".claude.json.tmp-claude-print-") && session_src.contains("0o600"),
        "the implementation must write the documented temp name and default mode"
    );
    for phrase in [
        "hasTrustDialogAccepted",
        ".claude.json.tmp-claude-print-<pid>",
        "0600",
        "leaving it untouched (trust scanner remains active)",
        "MOCK_TRUST_FROM_CLAUDE_JSON",
        "docs/notes/claude-p-compat-contract.md",
    ] {
        assert!(
            doc.contains(phrase),
            "the contract doc must pin {phrase:?} — the test suite and the doc \
             describe the same implementation"
        );
    }
}

// ── §3 + §6: trusted vs untrusted directories, observed end to end ──────────

/// The §6 trusted/untrusted A/B, plus the §3 fresh-file schema. The control
/// (no flag) runs FIRST against the untouched world: with the mock reading the
/// trust file, an untrusted cwd renders the unresolvable dialog and the run
/// must exit 2 — proving the knob is live and the directory genuinely
/// untrusted. Then the `--pretrust-cwd` run of the same world must succeed:
/// the dialog could only have been suppressed by the grant claude-print wrote
/// before spawn, and the file must carry exactly the §3 shape at mode 0600.
#[test]
fn observed_pretrust_grants_trust_and_suppresses_the_dialog() {
    let world = World::new();
    assert!(
        !world.claude_json().exists(),
        "the world must start untrusted: no trust file"
    );

    // Control: untrusted cwd → dialog renders → exit 2 naming the dialog and
    // the escape hatch. Nothing may be written by the control run.
    let mut control = world.cmd();
    world.args(&mut control);
    observe_dialog(&mut control);
    control.arg("test prompt");
    let out = run(&mut control, BUDGET, Stdio::null());
    assert_eq!(
        out.code,
        Some(2),
        "an untrusted cwd must render the unresolvable dialog and exit 2\n\
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("trust dialog") && out.stderr.contains("--pretrust-cwd"),
        "the refusal must name the trust dialog and the --pretrust-cwd escape \
         hatch:\n{}",
        out.stderr
    );
    assert!(
        !world.claude_json().exists(),
        "the control run must not create the trust file"
    );

    // The flag run: same world, same knobs — the grant lands before spawn, the
    // dialog cannot render, the session completes.
    let mut flagged = world.cmd();
    world.args(&mut flagged);
    observe_dialog(&mut flagged);
    flagged.arg("--pretrust-cwd");
    flagged.arg("test prompt");
    let out = run(&mut flagged, BUDGET, Stdio::null());
    assert_eq!(
        out.code,
        Some(0),
        "--pretrust-cwd must suppress the dialog and complete the session\n\
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("Hello from mock_claude"),
        "the session must have completed normally\nstdout:\n{}",
        out.stdout
    );

    // §3: the file carries exactly the documented shape at mode 0600.
    let value = trust_file_value(&world);
    let projects = value
        .get("projects")
        .and_then(|p| p.as_object())
        .expect("projects must be an object");
    let key = world.cwd_key();
    assert_eq!(
        projects
            .get(&key)
            .and_then(|e| e.get("hasTrustDialogAccepted")),
        Some(&serde_json::json!(true)),
        "the grant must sit at projects[<resolved cwd>].hasTrustDialogAccepted; \
         key used: {key:?}, file: {value}"
    );
    assert_eq!(
        projects.len(),
        1,
        "a fresh trust file must contain exactly the granted project: {projects:?}"
    );
    assert_eq!(
        value.as_object().expect("root must be an object").len(),
        1,
        "a fresh trust file must contain no scaffolding beyond projects: {value}"
    );
    let mode = fs::metadata(world.claude_json())
        .expect("trust file metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "a freshly created trust file must be mode 0600 (§3)"
    );
    assert_no_tmp_leftover(&world);
}

/// §6: the flag is claude-print-local. A run that demonstrably wrote the grant
/// must still show a child argv without the flag anywhere on it — and in the
/// documented default shape, so the recording cannot have slipped.
#[test]
fn observed_pretrust_flag_never_reaches_the_child_argv() {
    let world = World::new();
    let record = world.home_path().join("recorded-argv");
    let mut cmd = world.cmd();
    world.args(&mut cmd);
    cmd.env("MOCK_RECORD_ARGS", &record);
    cmd.arg("--pretrust-cwd");
    cmd.arg("test prompt");
    let out = run(&mut cmd, BUDGET, Stdio::null());
    assert_eq!(
        out.code,
        Some(0),
        "the run must succeed\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );

    // Non-vacuity: this very run wrote the grant, so the flag was live.
    let value = trust_file_value(&world);
    assert_eq!(
        value["projects"][world.cwd_key()]["hasTrustDialogAccepted"],
        serde_json::json!(true),
        "the run must have written the grant for the recording to mean anything"
    );

    let argv = read_recorded_argv(&record);
    assert!(
        argv.iter().any(|a| a.starts_with("--settings=")),
        "recording sanity: the child argv must carry the relay --settings= flag: \
         {argv:?}"
    );
    assert!(
        argv.windows(2)
            .any(|w| w == ["--model", "claude-sonnet-4-6"]),
        "recording sanity: the documented default --model pair must be present: \
         {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a.contains("pretrust")),
        "--pretrust-cwd must never reach the child argv in any spelling: {argv:?}"
    );
}

// ── §4: merge semantics and idempotency ─────────────────────────────────────

/// §4: a populated trust file keeps every sibling project, unrelated root key,
/// and unrelated field of the cwd's own entry while gaining the grant; the
/// merge canonicalizes the document to one sorted compact line; a pre-existing
/// mode survives; and a second run over the merged file is byte-identical.
#[test]
fn observed_merge_preserves_state_and_is_byte_idempotent() {
    let world = World::new();
    let sibling_entry = r#"{"hasTrustDialogAccepted": false, "allowedTools": ["Bash"]}"#;
    let own_entry = r#"{"lastCost": 1.25}"#;
    let pretty = format!(
        "{{
  \"oauthAccount\": {{\"safe\": \"value\"}},
  \"projects\": {{
    \"/somewhere/else\": {sibling_entry},
    \"{cwd}\": {own_entry}
  }},
  \"tipsHistory\": {{\"onboarding\": 1}}
}}",
        cwd = world.cwd_key()
    );
    fs::write(world.claude_json(), &pretty).expect("write populated trust file");
    fs::set_permissions(world.claude_json(), std::fs::Permissions::from_mode(0o644))
        .expect("set populated trust file mode");

    let mut first = world.cmd();
    world.args(&mut first);
    first.arg("--pretrust-cwd");
    first.arg("test prompt");
    let out = run(&mut first, BUDGET, Stdio::null());
    assert_eq!(
        out.code,
        Some(0),
        "the merge run must succeed\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );

    // Values preserved, grant added.
    let content_after_first = fs::read_to_string(world.claude_json()).expect("read merged file");
    let value: serde_json::Value =
        serde_json::from_str(&content_after_first).expect("merged trust file must stay valid JSON");
    let projects = value
        .get("projects")
        .and_then(|p| p.as_object())
        .expect("projects must survive the merge");
    assert_eq!(
        projects
            .get("/somewhere/else")
            .and_then(|e| e.get("hasTrustDialogAccepted")),
        Some(&serde_json::json!(false)),
        "the sibling project's trust state must survive untouched"
    );
    assert_eq!(
        projects
            .get("/somewhere/else")
            .and_then(|e| e.get("allowedTools")),
        Some(&serde_json::json!(["Bash"])),
        "the sibling project's unrelated fields must survive"
    );
    assert_eq!(
        projects
            .get(world.cwd_key().as_str())
            .and_then(|e| e.get("lastCost")),
        Some(&serde_json::json!(1.25)),
        "the cwd entry's unrelated fields must survive the merge"
    );
    assert_eq!(
        projects
            .get(world.cwd_key().as_str())
            .and_then(|e| e.get("hasTrustDialogAccepted")),
        Some(&serde_json::json!(true)),
        "the cwd entry must gain the grant"
    );
    assert_eq!(
        value.get("oauthAccount"),
        Some(&serde_json::json!({"safe": "value"})),
        "unrelated root keys must survive the merge"
    );
    assert_eq!(
        value.get("tipsHistory"),
        Some(&serde_json::json!({"onboarding": 1})),
        "unrelated root keys must survive the merge"
    );

    // Format canonicalized: one compact line, root keys lexicographically
    // sorted (§4's honest reserialization statement).
    assert_eq!(
        content_after_first.trim().lines().count(),
        1,
        "the merged document must be one compact line: {content_after_first:?}"
    );
    let oauth_at = content_after_first
        .find("\"oauthAccount\"")
        .expect("oauth key");
    let projects_at = content_after_first
        .find("\"projects\"")
        .expect("projects key");
    let tips_at = content_after_first
        .find("\"tipsHistory\"")
        .expect("tips key");
    assert!(
        oauth_at < projects_at && projects_at < tips_at,
        "serde_json emits lexicographically sorted keys; the doc pins this \
         canonical form: {content_after_first}"
    );

    // Mode preserved, not reset.
    let mode = fs::metadata(world.claude_json())
        .expect("trust file metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o644,
        "a pre-existing mode must be preserved across the merge (§4)"
    );

    // Idempotency: a second run over the merged file is byte-identical.
    let mut second = world.cmd();
    world.args(&mut second);
    second.arg("--pretrust-cwd");
    second.arg("test prompt");
    let out = run(&mut second, BUDGET, Stdio::null());
    assert_eq!(
        out.code,
        Some(0),
        "the idempotency run must succeed\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    let content_after_second = fs::read_to_string(world.claude_json()).expect("read remerged file");
    assert_eq!(
        content_after_second, content_after_first,
        "a second --pretrust-cwd run over the same state must be byte-identical (§4)"
    );
    assert_no_tmp_leftover(&world);
}

// ── §5: the soft failure rows — warn, leave byte-identical, proceed ─────────

/// Shared body of the three soft-failure rows: the poisoned trust file must be
/// byte-identical after the run, the run must still succeed through the
/// scanner-dismissed dialog (the mock reads the file as untrusted, so the
/// dialog renders with standard wording), the documented warning must be on
/// stderr, and no temporary file may be left behind. `original` is the poisoned
/// content, captured by the caller — the permission-denied row's file cannot be
/// read back inside here without the mode restore below.
fn assert_soft_failure_leaves_file_untouched(
    world: &World,
    warning_fragment: &str,
    original: &[u8],
) {
    let mut cmd = world.cmd();
    world.args(&mut cmd);
    cmd.env("MOCK_TRUST_FROM_CLAUDE_JSON", "1");
    cmd.arg("--pretrust-cwd");
    cmd.arg("test prompt");
    let out = run(&mut cmd, BUDGET, Stdio::null());

    assert_eq!(
        out.code,
        Some(0),
        "the soft-failure path must proceed through the scanner-dismissed dialog\n\
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("Hello from mock_claude"),
        "the session must have completed normally\nstdout:\n{}",
        out.stdout
    );
    assert!(
        out.stderr.contains("claude-print: warning: ~/.claude.json"),
        "the run must warn about the trust file on stderr:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains(warning_fragment),
        "the warning must name the failure shape ({warning_fragment:?}):\n{}",
        out.stderr
    );
    assert!(
        out.stderr
            .contains("leaving it untouched (trust scanner remains active)"),
        "the warning must say the file was left untouched:\n{}",
        out.stderr
    );
    // Restore access before the byte-identity read (a no-op for the readable
    // rows; the only mode this suite poisons is the permission row's 000).
    let _ = fs::set_permissions(world.claude_json(), std::fs::Permissions::from_mode(0o644));
    assert_eq!(
        fs::read(world.claude_json()).expect("read trust file after run"),
        original,
        "the trust file must be byte-identical after the soft failure (§5)"
    );
    assert_no_tmp_leftover(world);
}

/// §5 row 3: an unparseable trust file.
#[test]
fn observed_unparseable_file_is_left_untouched_behind_a_warning() {
    let world = World::new();
    let original = b"{ not valid json";
    fs::write(world.claude_json(), original).expect("write unparseable file");
    assert_soft_failure_leaves_file_untouched(&world, "is unreadable", original);
}

/// §5 row 4: valid JSON whose root is not an object.
#[test]
fn observed_non_object_root_is_left_untouched_behind_a_warning() {
    let world = World::new();
    let original = b"[1, 2, 3]";
    fs::write(world.claude_json(), original).expect("write non-object file");
    assert_soft_failure_leaves_file_untouched(&world, "is not a JSON object", original);
}

/// §5 row 5: a read error that is not absence (permission denied). The file
/// must never be papered over with a fresh-file rename — only genuine absence
/// reads as fresh. Skipped under root, which reads through mode 000 and would
/// take the merge path instead.
#[test]
fn observed_permission_denied_file_is_left_untouched_behind_a_warning() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root reads through mode 000, the scenario cannot be built");
        return;
    }
    let world = World::new();
    let original: &[u8] = br#"{"projects":{"/elsewhere":{"hasTrustDialogAccepted":true}}}"#;
    fs::write(world.claude_json(), original).expect("write unreadable file");
    fs::set_permissions(world.claude_json(), std::fs::Permissions::from_mode(0o000))
        .expect("chmod trust file to 000");
    assert_soft_failure_leaves_file_untouched(&world, "is unreadable", original);
}

// ── §5: the hard failure rows — exit 2, no write, no child spawned ──────────

/// Shared body of the two conflicting-shape rows: the run exits 2, the trust
/// file is byte-identical, and no child was ever spawned (nothing under the
/// home's `.claude` tree exists — only a running child creates transcripts
/// there). `mode` picks the rendering: text prints `error: …` on stderr, json
/// emits the structured `internal_error` object on stdout.
fn assert_conflicting_shape_hard_fails(world: &World, error_fragment: &str, mode: &str) {
    let original = fs::read(world.claude_json()).expect("read poisoned trust file");
    let mut cmd = world.cmd();
    world.args(&mut cmd);
    if mode == "json" {
        cmd.arg("--output-format").arg("json");
    }
    cmd.arg("--pretrust-cwd");
    cmd.arg("test prompt");
    let out = run(&mut cmd, BUDGET, Stdio::null());

    assert_eq!(
        out.code,
        Some(2),
        "a conflicting projects shape must hard-fail with exit 2 ({mode} mode)\n\
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert_eq!(
        fs::read(world.claude_json()).expect("read trust file after run"),
        original,
        "the hard-failure path must not write the trust file (§5)"
    );
    assert_no_tmp_leftover(world);
    assert!(
        !world.home_path().join(".claude").exists(),
        "no child may ever have been spawned: the home's .claude tree (which only \
         a running child populates) must not exist"
    );
    if mode == "text" {
        assert!(
            out.stderr.contains("error: ") && out.stderr.contains(error_fragment),
            "text mode must print the error line on stderr ({error_fragment:?}):\n{}",
            out.stderr
        );
    } else {
        assert!(
            out.stdout.contains("\"subtype\":\"internal_error\"")
                && out.stdout.contains(error_fragment),
            "json mode must emit the structured internal_error object on stdout \
             ({error_fragment:?}):\n{}",
            out.stdout
        );
        assert!(
            !out.stdout.contains("Hello from mock_claude"),
            "no session result may be emitted on the hard-failure path:\n{}",
            out.stdout
        );
    }
}

/// §5 row 6: parseable file, `projects` not an object — text rendering.
#[test]
fn observed_conflicting_projects_shape_hard_fails_without_writing() {
    let world = World::new();
    fs::write(world.claude_json(), r#"{"projects": "not an object"}"#)
        .expect("write conflicting file");
    assert_conflicting_shape_hard_fails(&world, "projects is not an object", "text");
}

/// §5 row 7: the cwd's own `projects` entry is not an object — json rendering,
/// pinning the structured error shape on stdout with the file still untouched.
#[test]
fn observed_conflicting_entry_shape_hard_fails_in_json_mode() {
    let world = World::new();
    let conflicting = format!(
        r#"{{"projects":{{"{}": "not an object"}}}}"#,
        world.cwd_key()
    );
    fs::write(world.claude_json(), conflicting).expect("write conflicting file");
    assert_conflicting_shape_hard_fails(&world, "project entry is not an object", "json");
}
