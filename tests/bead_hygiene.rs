//! Bead-workflow hygiene guard (bead claudepr-7e3c3454).
//!
//! AGENTS.md §"Bead workflow" states two rules that until now lived only in
//! prose, with no test or workflow enforcing either: every repository change
//! must be covered by an owning bead, and the durable checkpoint must be
//! refreshed (`bead sync flush-only`) after bead mutations before the work is
//! committed or pushed. `scripts/check-bead-hygiene.sh` is their executable
//! owner — a safe pre-commit / pre-push / CI gate — and these tests pin it:
//!
//! - wiring: the script exists, is executable, carries the `env bash` shebang
//!   this NixOS host requires, names its three checks and flags, and contains
//!   no destructive bead subcommand in its source;
//! - backend: the `.needle.yaml` declaration must agree with the on-disk
//!   tells (bead-rs = `.beads/config.json` + `.beads/checkpoint/`; bf =
//!   `.beads/config.yaml` + flat `issues.jsonl`). Ambiguity in either
//!   direction fails closed exit 2 — the "stop and re-check the backend
//!   declaration before attempting any repair" posture, because the wrong CLI
//!   against a store does not fail cleanly (the 2026-08-14 SEAM incident);
//! - ownership: a commit whose subject references no `claudepr-*` bead, or
//!   whose reference does not resolve in the live store, is drift exit 1;
//!   uncommitted non-`.beads` changes need `--worktree-bead` or a
//!   self-attributing `notes/<prefix>-<id>.md` journal;
//! - checkpoint: staged (or committed-in-range) `.beads/checkpoint/` files
//!   are drift exit 1 — the Forgejo pre-receive gitleaks hook rejects
//!   checkpoint commits in this repo (generic-api-key false positive on
//!   immutable closed-bead close-reason prose, verified 2026-09-18 during
//!   claudepr-2069ca6e) — while *uncommitted* checkpoint drift is tolerated,
//!   because flushing and leaving the drift in the working tree is the
//!   policy. A stale checkpoint is detected by probing a throwaway COPY:
//!   `bead sync flush-only` must run somewhere other than the workspace and
//!   the workspace's checkpoint bytes must be unchanged by any run;
//! - safety (always-on, asserted on every scenario that reaches the store):
//!   the stubbed `bead` is only ever invoked as `show <id>` or
//!   `sync flush-only`, the sync lands under a temp probe directory — never
//!   inside the workspace — and the store survives byte-identical.
//!
//! Hermetic after the `tests/contract_maintenance.rs` pattern: each scenario
//! runs in its own temp fixture (a git repo with a bare remote for the
//! upstream default), reached through a single-entry PATH of real-coreutils
//! symlinks plus a stub `bead` whose `show` resolves exactly two fixture IDs
//! and whose `sync flush-only` is controllable (mutate-on-sync makes the
//! checkpoint stale; fail-on-sync makes the probe indeterminate).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The script under test, repo-relative.
const SCRIPT: &str = "scripts/check-bead-hygiene.sh";

/// Repo-root probes: a directory holding both is a claude-print checkout.
const ROOT_PROBES: [&str; 2] = ["AGENTS.md", "Cargo.toml"];

/// The two fixture bead IDs the stub `bead show` resolves.
const SEED_BEAD: &str = "claudepr-11111111";
const OTHER_BEAD: &str = "claudepr-22222222";

/// The seed checkpoint content every fixture starts from — the byte-stability
/// assertion in [`assert_store_untouched`] pins this exact string.
const SEED_CHECKPOINT: &str = "gen-fixture-1\n";

/// Every binary the script itself reaches on its stubbed PATH: the tools its
/// own `command -v` audit names, `dirname` (the REPO_ROOT resolution on its
/// second code line, before any audit), and `bash` (the stub `bead`'s
/// shebang resolves `bash` through the child's PATH).
const SCRIPT_TOOLS: &[&str] = &[
    "bash",
    "dirname",
    "git",
    "grep",
    "sed",
    "sort",
    "comm",
    "head",
    "sha256sum",
    "mktemp",
    "cp",
    "rm",
    "find",
    "diff",
];

/// Where repo-relative files are read from, resolved at *runtime* — never
/// the bare compile-time `env!("CARGO_MANIFEST_DIR")`, which bakes the
/// building checkout's path into the test binary. The local cargo wrapper
/// maps `.git`-less `git archive` extractions onto one shared target dir,
/// so an extraction of unchanged content instant-reuses a cached test
/// binary compiled in an extraction that has since been deleted; a
/// baked-only root then fails every later run of that binary with
/// file-NotFound panics that have nothing to do with drift (bead
/// claudepr-270570be; the same chain as `tests/install_sh.rs`).
fn repo_path(relative: &str) -> PathBuf {
    resolve_repo_root(
        std::env::var("CLAUDE_PRINT_TEST_REPO").ok().as_deref(),
        std::env::var("CARGO_MANIFEST_DIR").ok().as_deref(),
        env!("CARGO_MANIFEST_DIR"),
    )
    .unwrap_or_else(|e| panic!("locating the repo root to read {relative} from: {e}"))
    .join(relative)
}

/// [`repo_path`]'s candidate chain as a pure function, so the precedence
/// and the loud failure are testable without racing the process-wide
/// environment from parallel tests.
fn resolve_repo_root(
    env_override: Option<&str>,
    runtime_manifest: Option<&str>,
    baked_manifest: &str,
) -> Result<PathBuf, String> {
    if let Some(override_root) = env_override {
        if is_repo_root(Path::new(override_root)) {
            return Ok(PathBuf::from(override_root));
        }
        return Err(format!(
            "$CLAUDE_PRINT_TEST_REPO={override_root:?} is set but not a claude-print \
             checkout (probe: {ROOT_PROBES:?}) — an explicit override is authoritative \
             and is never silently skipped for another candidate"
        ));
    }
    let mut chain: Vec<(&str, &str)> = vec![("compile-time", baked_manifest)];
    if let Some(runtime) = runtime_manifest {
        if runtime != baked_manifest {
            chain.insert(0, ("runtime", runtime));
        }
    }
    let mut rejected = Vec::new();
    for (origin, candidate) in chain {
        let path = Path::new(candidate);
        if is_repo_root(path) {
            return Ok(path.to_path_buf());
        }
        rejected.push(format!("{origin} CARGO_MANIFEST_DIR={}", path.display()));
    }
    Err(format!(
        "no candidate repo root is a claude-print checkout (probe: {ROOT_PROBES:?}): {} — \
         run via `cargo test` from a checkout, or set $CLAUDE_PRINT_TEST_REPO to one",
        rejected.join("; ")
    ))
}

/// Whether `p` holds this suite's root probes.
fn is_repo_root(p: &Path) -> bool {
    ROOT_PROBES.iter().all(|f| p.join(f).is_file())
}

// ── hermetic stub PATH ───────────────────────────────────────────────────────

/// Symlink the real utilities the script needs into the stub bin dir, so a
/// single-entry PATH is self-contained. Each tool is located by scanning the
/// test process's PATH for an existing entry (NixOS resolves coreutils
/// through profile symlinks, so the scan must follow them). Idempotent:
/// existing links (and stubs) are left alone.
fn link_tools(bin: &Path, tools: &[&str]) {
    fs::create_dir_all(bin).unwrap();
    for tool in tools {
        let dest = bin.join(tool);
        if dest.symlink_metadata().is_ok() {
            continue;
        }
        let real = std::env::var_os("PATH")
            .and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join(tool))
                    .find(|p| p.is_file())
            })
            .unwrap_or_else(|| panic!("{tool} must be reachable to build the stub bin dir"));
        std::os::unix::fs::symlink(&real, &dest).unwrap();
    }
}

fn write_executable(dir: &Path, name: &str, body: &str) {
    fs::create_dir_all(dir).unwrap();
    let stub = dir.join(name);
    fs::write(&stub, format!("#!/usr/bin/env bash\n{body}\n")).unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A `bead` whose `show` resolves exactly the two fixture IDs and whose
/// `sync flush-only` is test-controlled. Every invocation is recorded to
/// `$BEAD_ARGS_FILE` as `cwd=<pwd> args=<argv joined>` — the evidence the
/// safety assertions read. `BEAD_SYNC_MUTATE=1` makes flush-only append to
/// the COPY's checkpoint (simulating a suppressed auto-publish: the store
/// has mutations the checkpoint lacks); `BEAD_SYNC_FAIL=1` makes it exit 7
/// (simulating a torn live store or a broken probe).
fn write_stub_bead(dir: &Path) {
    write_executable(
        dir,
        "bead",
        r#"set -u
printf '%s\n' "cwd=$(pwd) args=$*" >> "${BEAD_ARGS_FILE:?BEAD_ARGS_FILE not set}"
case "$1" in
    show)
        if [ "$2" = "CLAUDEPR_SEED" ] || [ "$2" = "CLAUDEPR_OTHER" ]; then
            exit 0
        fi
        echo "Issue not found: $2" >&2
        exit 1
        ;;
    sync)
        if [ "$2" != "flush-only" ]; then
            echo "stub bead: unexpected sync argument: $*" >&2
            exit 64
        fi
        if [ "${BEAD_SYNC_MUTATE:-0}" = 1 ]; then
            printf 'stale-gen-line\n' >> .beads/checkpoint/current.json
        fi
        exit "${BEAD_SYNC_FAIL:-0}"
        ;;
    *)
        echo "stub bead: unexpected subcommand: $*" >&2
        exit 64
        ;;
esac
"#,
    );
}

/// The seed/other ID substitution into the stub above (kept out of the body
/// so the body stays readable).
fn install_stub_bead(bin: &Path) {
    write_stub_bead(bin);
    let stub = bin.join("bead");
    let body = fs::read_to_string(&stub)
        .unwrap()
        .replace("CLAUDEPR_SEED", SEED_BEAD)
        .replace("CLAUDEPR_OTHER", OTHER_BEAD);
    fs::write(&stub, body).unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
}

// ── fixture ──────────────────────────────────────────────────────────────────

/// One self-contained bead-rs workspace: a git repo on `main` pushed to a
/// bare remote (so the default `@{upstream}..HEAD` range resolves), a
/// committed seed carrying `(<seed bead>)` in its subject, and the script
/// copied inside (it resolves its repo root from its own location).
struct Fixture {
    /// Keeps the temp tree alive for the life of the scenario.
    _root: tempfile::TempDir,
    ws: PathBuf,
    bin: PathBuf,
    bead_args: PathBuf,
}

impl Fixture {
    fn build() -> Fixture {
        Self::build_with_seed(&format!("seed({SEED_BEAD}): workspace seed"))
    }

    fn build_with_seed(subject: &str) -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let ws = root.path().join("ws");
        let bin = root.path().join("bin");
        fs::create_dir_all(ws.join("scripts")).unwrap();
        fs::create_dir_all(ws.join(".beads/checkpoint")).unwrap();
        let script = ws.join(SCRIPT);
        fs::copy(repo_path(SCRIPT), &script).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(ws.join(".needle.yaml"), "bead_cli:\n  backend: bead-rs\n").unwrap();
        fs::write(
            ws.join(".beads/config.json"),
            "{\"created_at\":\"2026-01-01T00:00:00Z\",\"prefix\":\"claudepr\",\"uuid\":\"fixture\"}\n",
        )
        .unwrap();
        fs::write(ws.join(".beads/checkpoint/current.json"), SEED_CHECKPOINT).unwrap();

        link_tools(&bin, SCRIPT_TOOLS);
        install_stub_bead(&bin);

        let bead_args = root.path().join("bead-args.log");
        let fixture = Fixture {
            _root: root,
            ws,
            bin,
            bead_args,
        };
        fixture.git(&["init", "-q", "-b", "main"]);
        fixture.git(&["add", "-A"]);
        fixture.commit(subject);
        let remote = fixture._root.path().join("remote.git");
        let out = Command::new("git")
            .arg("init")
            .arg("-q")
            .arg("--bare")
            .arg(&remote)
            .current_dir(fixture._root.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "bare remote init failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        fixture.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
        fixture.git(&["push", "-q", "-u", "origin", "main"]);
        fixture
    }

    /// Run git in the workspace with a fixed identity; panics on failure with
    /// the captured output.
    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.ws)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit(&self, subject: &str) {
        self.git(&[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.com",
            "commit",
            "-qm",
            subject,
        ]);
    }

    /// Commit every current change (fixtures construct known state).
    fn commit_all(&self, subject: &str) {
        self.git(&["add", "-A"]);
        self.commit(subject);
    }

    fn write(&self, rel: &str, contents: &str) {
        let path = self.ws.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    /// Run the script through the hermetic PATH from the fixture temp root —
    /// cwd is deliberately *not* the workspace, pinning that the script
    /// resolves its repo root from its own location.
    fn run(&self, args: &[&str]) -> Output {
        self.run_with_env(args, &[])
    }

    fn run_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new("bash");
        cmd.arg(self.ws.join(SCRIPT))
            .env("PATH", self.bin.display().to_string())
            .env("BEAD_ARGS_FILE", &self.bead_args)
            .current_dir(self._root.path());
        for (key, value) in envs {
            cmd.env(key, value);
        }
        cmd.args(args).output().unwrap()
    }

    fn checkpoint_bytes(&self) -> String {
        fs::read_to_string(self.ws.join(".beads/checkpoint/current.json")).unwrap()
    }

    fn bead_log(&self) -> String {
        fs::read_to_string(&self.bead_args).unwrap_or_default()
    }
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// The always-on safety contract, asserted on every scenario that gets far
/// enough to touch the store: the stub `bead` was invoked only as
/// `show <id>` / `sync flush-only`, no `sync` ran inside the workspace (the
/// probe's copy isolation), and the workspace checkpoint is byte-identical to
/// `expected` — the seed content for every scenario, and the pre-written
/// drift for the one that deliberately carries it.
fn assert_store_untouched(fixture: &Fixture, expected: &str) {
    for line in fixture.bead_log().lines() {
        let (cwd, args) = line
            .split_once(" args=")
            .unwrap_or_else(|| panic!("unparseable stub invocation: {line}"));
        let show_allowed = args.starts_with("show claudepr-");
        assert!(
            show_allowed || args == "sync flush-only",
            "bead was invoked with something other than the read/show or copy-probe \
             allowlist: {args:?}"
        );
        if args == "sync flush-only" {
            assert!(
                !Path::new(cwd).starts_with(&fixture.ws),
                "the freshness probe ran inside the workspace, not a throwaway copy: {cwd}"
            );
        }
    }
    if fixture.ws.join(".beads/checkpoint/current.json").exists() {
        assert_eq!(
            fixture.checkpoint_bytes(),
            expected,
            "the workspace checkpoint changed during a run that only inspects"
        );
    }
}

// ── wiring ───────────────────────────────────────────────────────────────────

#[test]
fn script_is_wired_and_documents_its_safety_contract() {
    let meta = fs::metadata(repo_path(SCRIPT)).expect("the script must exist");
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "the script must be executable"
    );
    let script = fs::read_to_string(repo_path(SCRIPT)).unwrap();
    assert!(
        script.starts_with("#!/usr/bin/env bash\n"),
        "NixOS has no /bin/bash — the shebang must be `#!/usr/bin/env bash`"
    );
    assert!(script.contains("set -u"), "unbound variables must fail");
    for needle in [
        "backend",
        "ownership",
        "checkpoint",
        "--worktree-bead",
        "--range",
        "--skip",
        "bead sync flush-only",
        "bead show",
    ] {
        assert!(script.contains(needle), "the script must name {needle:?}");
    }
    for forbidden in ["bead init", "import-only", "doctor --repair", "kv delete"] {
        assert!(
            !script.contains(forbidden),
            "the script must never carry the destructive subcommand {forbidden:?}"
        );
    }
}

#[test]
fn repo_root_resolution_prefers_the_runtime_manifest() {
    let baked = env!("CARGO_MANIFEST_DIR");
    // Runtime value wins when it differs and is a real checkout.
    let resolved = resolve_repo_root(None, Some(baked), "/nonexistent/baked").unwrap();
    assert!(resolved.ends_with("claude-print") || is_repo_root(&resolved));
    // An override that is not a checkout is authoritative and loud.
    let err = resolve_repo_root(Some("/tmp"), None, baked).unwrap_err();
    assert!(err.contains("CLAUDE_PRINT_TEST_REPO"));
    // Nothing viable names every candidate it rejected.
    let err = resolve_repo_root(None, Some("/nonexistent"), "/also/missing").unwrap_err();
    assert!(err.contains("/nonexistent") && err.contains("/also/missing"));
}

// ── the pass path ────────────────────────────────────────────────────────────

#[test]
fn clean_bead_rs_workspace_passes_end_to_end() {
    let fixture = Fixture::build();
    fixture.write("docs/notes/clean.md", "content\n");
    fixture.commit_all(&format!("docs({OTHER_BEAD}): referenced follow-up"));
    let out = fixture.run(&[]);
    assert_eq!(
        code(&out),
        0,
        "stdout:\n{}\nstderr:\n{}",
        stdout_of(&out),
        stderr_of(&out)
    );
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("backend: declared 'bead-rs'"),
        "stdout was:\n{stdout}"
    );
    assert!(
        stdout.contains("1 commit(s) in origin/main..HEAD"),
        "stdout was:\n{stdout}"
    );
    assert!(
        stdout.contains("durable checkpoint is current"),
        "stdout:\n{stdout}"
    );
    assert_store_untouched(&fixture, SEED_CHECKPOINT);
}

// ── backend ──────────────────────────────────────────────────────────────────

#[test]
fn backend_declaration_contradicted_by_tells_fails_closed() {
    let fixture = Fixture::build();
    fixture.write(".needle.yaml", "bead_cli:\n  backend: bf\n");
    let out = fixture.run(&[]);
    assert_eq!(code(&out), 2, "stderr:\n{}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("the on-disk tells say 'bead-rs'"),
        "stderr was:\n{}",
        stderr_of(&out)
    );
    assert_store_untouched(&fixture, SEED_CHECKPOINT);
}

#[test]
fn store_without_declaration_fails_closed() {
    let fixture = Fixture::build();
    fs::remove_file(fixture.ws.join(".needle.yaml")).unwrap();
    let out = fixture.run(&[]);
    assert_eq!(code(&out), 2, "stderr:\n{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("declares no 'backend:'"));
}

#[test]
fn ambiguous_backend_tells_fail_closed() {
    let fixture = Fixture::build();
    fixture.write(".beads/config.yaml", "prefix: claudepr\n");
    fixture.write(".beads/issues.jsonl", "");
    let out = fixture.run(&[]);
    assert_eq!(code(&out), 2, "stderr:\n{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("ambiguous"));
}

#[test]
fn declaration_without_store_fails_closed_but_storeless_repo_passes() {
    // A declaration with no store is an inconsistency...
    let fixture = Fixture::build();
    fs::remove_dir_all(fixture.ws.join(".beads")).unwrap();
    let out = fixture.run(&[]);
    assert_eq!(code(&out), 2, "stderr:\n{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("declaration without a store"));

    // ...while a repo with neither store nor declaration is simply out of
    // the rules' scope.
    let fixture = Fixture::build();
    fs::remove_dir_all(fixture.ws.join(".beads")).unwrap();
    fs::remove_file(fixture.ws.join(".needle.yaml")).unwrap();
    let out = fixture.run(&[]);
    assert_eq!(code(&out), 0, "stderr:\n{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("no bead store"));
}

// ── ownership ────────────────────────────────────────────────────────────────

#[test]
fn commit_without_bead_reference_is_drift() {
    let fixture = Fixture::build();
    fixture.write("docs/notes/x.md", "content\n");
    fixture.commit_all("docs: free-floating change with no owner");
    let out = fixture.run(&[]);
    assert_eq!(code(&out), 1, "stderr:\n{}", stderr_of(&out));
    let stderr = stderr_of(&out);
    assert!(stderr.contains("reference no claudepr-"));
    assert!(stderr.contains("docs: free-floating change with no owner"));
    assert_store_untouched(&fixture, SEED_CHECKPOINT);
}

#[test]
fn commit_with_unresolvable_reference_is_drift() {
    let fixture = Fixture::build();
    fixture.write("src/thing.rs", "fn f() {}\n");
    fixture.commit_all("fix(claudepr-99999999): ghost reference");
    let out = fixture.run(&[]);
    assert_eq!(code(&out), 1, "stderr:\n{}", stderr_of(&out));
    let stderr = stderr_of(&out);
    assert!(stderr.contains("do not resolve"));
    assert!(stderr.contains("claudepr-99999999"));
    assert_store_untouched(&fixture, SEED_CHECKPOINT);
}

#[test]
fn explicit_range_is_honoured() {
    let fixture = Fixture::build();
    // A referenced commit, then an unreferenced tip on top of it.
    fixture.write("docs/notes/y.md", "content\n");
    fixture.commit_all(&format!("docs({OTHER_BEAD}): referenced change"));
    fixture.write("docs/notes/z.md", "content\n");
    fixture.commit_all("docs: free-floating tip");

    // The default range covers the free-floating tip and is drift...
    let out = fixture.run(&[]);
    assert_eq!(code(&out), 1);
    assert!(stderr_of(&out).contains("docs: free-floating tip"));

    // ...--range scopes the audit below it, and the referenced history
    // under the same script passes.
    let out = fixture.run(&["--range", "HEAD~2..HEAD~1"]);
    assert_eq!(code(&out), 0, "stderr:\n{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("1 commit(s) in HEAD~2..HEAD~1"));

    // And a range that resolves to nothing is pass, not error.
    let out = fixture.run(&["--range", "origin/main..origin/main"]);
    assert_eq!(code(&out), 0, "stderr:\n{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("no commits in"));
}

#[test]
fn worktree_change_needs_an_owner_unless_named() {
    let fixture = Fixture::build();
    fixture.write("src/wip.rs", "todo\n");
    let out = fixture.run(&[]);
    assert_eq!(code(&out), 1, "stderr:\n{}", stderr_of(&out));
    let stderr = stderr_of(&out);
    assert!(stderr.contains("uncommitted change(s) with no owning bead"));
    assert!(stderr.contains("src/wip.rs"));

    // Named via --worktree-bead, the same run passes.
    let out = fixture.run(&["--worktree-bead", SEED_BEAD]);
    assert_eq!(code(&out), 0, "stderr:\n{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("attributed to"));

    // A --worktree-bead that does not resolve is still drift.
    let out = fixture.run(&["--worktree-bead", "claudepr-99999999"]);
    assert_eq!(code(&out), 1);
    assert!(stderr_of(&out).contains("does not resolve"));
    assert_store_untouched(&fixture, SEED_CHECKPOINT);
}

#[test]
fn notes_journal_is_self_attributing_but_other_untracked_files_are_not() {
    let fixture = Fixture::build();
    fixture.write(&format!("notes/{SEED_BEAD}.md"), "worker journal entry\n");
    let out = fixture.run(&[]);
    assert_eq!(code(&out), 0, "stderr:\n{}", stderr_of(&out));

    fixture.write("scratch.txt", "stray\n");
    let out = fixture.run(&[]);
    assert_eq!(code(&out), 1);
    assert!(stderr_of(&out).contains("scratch.txt"));
    assert_store_untouched(&fixture, SEED_CHECKPOINT);
}

// ── checkpoint ───────────────────────────────────────────────────────────────

#[test]
fn stale_checkpoint_is_detected_on_a_copy_never_the_store() {
    let fixture = Fixture::build();
    fixture.write("src/wip.rs", "todo\n");
    // mutate-on-sync: the probe copy becomes stale the moment sync runs,
    // which is exactly the "auto-publish was suppressed" shape.
    let out = fixture.run_with_env(
        &["--worktree-bead", SEED_BEAD],
        &[("BEAD_SYNC_MUTATE", "1")],
    );
    assert_eq!(code(&out), 1, "stderr:\n{}", stderr_of(&out));
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("checkpoint is stale"),
        "stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains(".beads/checkpoint/current.json"),
        "stderr was:\n{stderr}"
    );
    assert!(stderr.contains("bead sync flush-only"));
    assert_store_untouched(&fixture, SEED_CHECKPOINT);
}

#[test]
fn probe_failure_fails_closed_as_indeterminate() {
    let fixture = Fixture::build();
    let out = fixture.run_with_env(&[], &[("BEAD_SYNC_FAIL", "1")]);
    assert_eq!(code(&out), 2, "stderr:\n{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("failed inside the copy"));
    assert_store_untouched(&fixture, SEED_CHECKPOINT);
}

#[test]
fn staged_checkpoint_file_is_drift() {
    let fixture = Fixture::build();
    fixture.write(".beads/checkpoint/current.json", "gen-fixture-2\n");
    fixture.git(&["add", ".beads/checkpoint/current.json"]);
    let out = fixture.run(&["--worktree-bead", SEED_BEAD]);
    assert_eq!(code(&out), 1, "stderr:\n{}", stderr_of(&out));
    let stderr = stderr_of(&out);
    assert!(stderr.contains("staged for commit"));
    assert!(stderr.contains("gitleaks"));
    assert_store_untouched(&fixture, "gen-fixture-2\n");
}

#[test]
fn checkpoint_commit_inside_the_range_is_drift() {
    let fixture = Fixture::build();
    fixture.write(".beads/checkpoint/current.json", "gen-fixture-2\n");
    fixture.commit_all(&format!("chore({SEED_BEAD}): sweep the checkpoint"));
    let out = fixture.run(&[]);
    assert_eq!(code(&out), 1, "stderr:\n{}", stderr_of(&out));
    let stderr = stderr_of(&out);
    assert!(stderr.contains("change .beads/checkpoint/"));
    assert!(stderr.contains("gitleaks"));
    assert_store_untouched(&fixture, "gen-fixture-2\n");
}

#[test]
fn uncommitted_checkpoint_drift_is_tolerated() {
    // Flushing and leaving the checkpoint uncommitted IS the policy here —
    // only staging or committing it is drift.
    let fixture = Fixture::build();
    fixture.write(".beads/checkpoint/current.json", "gen-fixture-2\n");
    let out = fixture.run(&["--worktree-bead", SEED_BEAD]);
    assert_eq!(code(&out), 0, "stderr:\n{}", stderr_of(&out));
    assert_store_untouched(&fixture, "gen-fixture-2\n");
}

// ── store-shape and flag interactions ────────────────────────────────────────

#[test]
fn consistent_bf_store_is_not_silently_waved_through() {
    // bf is retired in this environment, so a bf-shaped store is itself a
    // decision nobody has made — the ownership half fails closed rather
    // than auditing it with the wrong ID grammar, and the freshness probe
    // refuses to guess.
    let fixture = Fixture::build();
    fs::remove_file(fixture.ws.join(".beads/config.json")).unwrap();
    fs::remove_dir_all(fixture.ws.join(".beads/checkpoint")).unwrap();
    fixture.write(".needle.yaml", "bead_cli:\n  backend: bf\n");
    fixture.write(".beads/config.yaml", "prefix: claudepr\n");
    fixture.write(".beads/issues.jsonl", "");
    let out = fixture.run(&[]);
    assert_eq!(code(&out), 2, "stderr:\n{}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("prefix"),
        "stderr was:\n{}",
        stderr_of(&out)
    );
    assert_store_untouched(&fixture, SEED_CHECKPOINT);
}

#[test]
fn skip_flags_narrow_the_run_without_a_bead_binary() {
    // `--skip ownership --skip checkpoint` leaves backend only — which never
    // needs a bead CLI, so the run must pass with no `bead` on PATH at all.
    let fixture = Fixture::build();
    fs::remove_file(fixture.bin.join("bead")).unwrap();
    let out = fixture.run(&["--skip", "ownership", "--skip", "checkpoint"]);
    assert_eq!(
        code(&out),
        0,
        "stdout:\n{}\nstderr:\n{}",
        stdout_of(&out),
        stderr_of(&out)
    );
    assert!(stdout_of(&out).contains("backend: declared 'bead-rs'"));
    assert!(!stdout_of(&out).contains("ownership:"));
    assert!(!stdout_of(&out).contains("checkpoint:"));
}

#[test]
fn unknown_flag_fails_closed_with_usage() {
    let fixture = Fixture::build();
    let out = fixture.run(&["--nonsense"]);
    assert_eq!(code(&out), 2);
    assert!(stderr_of(&out).contains("unknown argument"));
}
