//! Documentation-contract test for `docs/notes/hook-design.md`
//! (bead claudepr-cddbb476).
//!
//! hook-design.md is the normative spec for the Stop FIFO +
//! UserPromptSubmit identity relay, but unlike the sibling contract docs
//! (billing-context.md, config-file-contract.md, home-handling-strategy.md,
//! pool-socket-protocol.md) it carried no provenance table, and its cited
//! tests pin *behavior*, not *document text*: `src/hook.rs`'s unit tests
//! would keep passing while a doc-only edit quietly misstated the contract.
//! This test is the drift guard — every check reads the doc and judges it
//! against the implementation, never the reverse:
//!
//!   * the provenance table at the top names this test, the implementing
//!     modules, and the owning bead;
//!   * the documented `<TMPDIR>/claude-print-<pid>-<rand>/` layout is
//!     re-derived from a real `HookInstaller` — the dir lands in
//!     `std::env::temp_dir()`, its name embeds *this* process's live pid
//!     (the premise the startup orphan sweep's liveness check rests on,
//!     cross-checked against the `hook::cleanup_orphans()` call at the top
//!     of `main()`), and the installer-created artifact set matches the
//!     documented tree, `session-identity.json` absent until the relay
//!     writes it;
//!   * the 0700 dir / 0600 stop.fifo / 0750 relay-script mode guarantees
//!     are re-derived by stat, including under a flipped hostile umask 000
//!     (the case the doc calls out);
//!   * the `write_cat_script` relay body is pinned byte-exact —
//!     `#!/bin/sh` + `cat > '<target>' 2>/dev/null || true` — against both
//!     generated scripts, and the `'\''` single-quote escaping is replayed
//!     for real by pointing TMPDIR at a directory whose path carries a
//!     quote: the generated script must contain the escape and unescape
//!     back to the exact FIFO/identity path;
//!   * the session-identity sibling invariant holds in the shape pool
//!     clients reconstruct it, `stop_fifo().with_file_name(SESSION_IDENTITY_FILE)`;
//!   * the per-run settings.json wiring matches the documented two-hook
//!     schema (closed world: Stop + UserPromptSubmit, command paths, the
//!     `timeout: 10` both blocks show) and the `--settings={}` argv
//!     forwarding exists in `src/session.rs` and `src/pool.rs`.
//!
//! What this guard deliberately does **not** pin: the transcript-slug
//! algorithm (owned by `tests/docs_slug_consistency.rs`), the measured
//! Claude Code runtime contracts (merge/suppression/Stop-frequency/timeout
//! — owned by `tests/claude_contracts.rs` and its fixtures), and the
//! cleanup/liveness *behavior* (owned by the `src/hook.rs` unit tests the
//! doc cites) — only the doc text describing them is held present.
//!
//! Library-level and hermetic: reads markdown and `src/`, constructs real
//! `HookInstaller`s, spawns nothing. The two process-global replays
//! (TMPDIR and umask) and every installer construction serialize on one
//! lock, so a flipped TMPDIR can never relocate another test's artifacts
//! mid-assertion.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{MutexGuard, OnceLock};

use claude_print::hook::{HookInstaller, SESSION_IDENTITY_FILE};

/// The doc under guard.
const DOC: &str = "docs/notes/hook-design.md";

/// Repo-root probes: a directory holding both is a claude-print checkout.
const ROOT_PROBES: [&str; 2] = ["AGENTS.md", "Cargo.toml"];

/// Where repo-relative files are read from, resolved at *runtime* — never
/// the bare compile-time `env!("CARGO_MANIFEST_DIR")`, which bakes the
/// building checkout's path into the test binary. The local cargo wrapper
/// maps `.git`-less `git archive` extractions onto one shared target dir,
/// so an extraction of unchanged content instant-reuses a cached test
/// binary compiled in an extraction that has since been deleted; a
/// baked-only root then fails every later run of that binary with
/// file-NotFound panics that have nothing to do with drift (bead
/// claudepr-270570be; the same chain as `tests/install_sh.rs`). Candidates, most
/// authoritative first, each probe-verified before use:
///
/// 1. `$CLAUDE_PRINT_TEST_REPO` — explicit override for direct binary runs;
///    when set it is authoritative and must itself be a checkout.
/// 2. the runtime `CARGO_MANIFEST_DIR` cargo sets in the test process to
///    the package under test — the live extraction even in a cache-reused
///    binary.
/// 3. the compile-time `CARGO_MANIFEST_DIR` — last resort for running the
///    test binary directly, where cargo sets neither variable.
///
/// If no candidate survives its probe the panic names every candidate it
/// rejected — loud, never a vacuous pass off a wrong tree.
fn repo_root() -> PathBuf {
    resolve_repo_root(
        std::env::var("CLAUDE_PRINT_TEST_REPO").ok().as_deref(),
        std::env::var("CARGO_MANIFEST_DIR").ok().as_deref(),
        env!("CARGO_MANIFEST_DIR"),
    )
    .unwrap_or_else(|e| panic!("locating the repo root to read repo files from: {e}"))
}

/// [`repo_root`]'s candidate chain as a pure function, so the precedence
/// and the loud failure are testable without racing the process-wide
/// environment from parallel tests (the same shape as
/// `tests/install_sh.rs`).
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
             checkout (probe: {:?} + {:?}) — an explicit override is authoritative and \
             is never silently skipped for another candidate",
            ROOT_PROBES[0], ROOT_PROBES[1]
        ));
    }
    // Runtime value first, baked value only as fallback; one chain so the
    // failure names everything that was tried.
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
        "no candidate repo root is a claude-print checkout (probe: {:?} + {:?}): {} — \
         run via `cargo test` from a checkout, or set $CLAUDE_PRINT_TEST_REPO to one",
        ROOT_PROBES[0],
        ROOT_PROBES[1],
        rejected.join("; ")
    ))
}

/// Whether `p` holds this suite's root probes.
fn is_repo_root(p: &Path) -> bool {
    ROOT_PROBES.iter().all(|f| p.join(f).is_file())
}

fn read_doc() -> String {
    fs::read_to_string(repo_root().join(DOC)).unwrap_or_else(|e| panic!("read {DOC}: {e}"))
}

/// The body of a `## <heading>` section: from just after the heading line
/// to the next `## ` heading (or EOF). Subsections (`###`) stay inside it.
fn section<'a>(md: &'a str, heading: &str) -> &'a str {
    let marker = format!("## {heading}");
    let start = md
        .find(&format!("\n{marker}\n"))
        .map(|p| p + 1)
        .or_else(|| md.starts_with(&marker).then_some(0))
        .unwrap_or_else(|| panic!("heading '{marker}' not found in {DOC}"));
    let body = md[start + marker.len()..]
        .strip_prefix('\n')
        .unwrap_or(&md[start + marker.len()..]);
    let end = body.find("\n## ").unwrap_or(body.len());
    &body[..end]
}

/// Whitespace-normalized text, so pinned phrases survive line re-wrapping.
fn normalized(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Serializes the process-global replays (TMPDIR flip, umask flip) and
/// every `HookInstaller` construction: a concurrent TMPDIR flip would
/// relocate another test's freshly created installer out from under its
/// `temp_dir()` parentage assertion.
fn global_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A path's permission bits, masked to the 0o777 the doc speaks in.
fn mode_of(path: &Path) -> u32 {
    fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o777
}

// ── the provenance table ─────────────────────────────────────────────────────

/// The table sits at the top of the doc — before the first `## ` heading —
/// and names the pinning test, the implementing modules, and the owning
/// bead, the same shape as billing-context.md / config-file-contract.md.
#[test]
fn provenance_table_names_the_pinning_test_modules_and_bead() {
    let doc = read_doc();
    let top = doc.split_once("\n## ").map(|(top, _)| top).unwrap_or(&doc);
    for cell in [
        "| **Pinned by** |",
        "| **Implementation** |",
        "| **Provenance** |",
        "tests/docs_hook_contract.rs",
        "`src/hook.rs`",
        "`src/pty.rs`",
        "bead claudepr-cddbb476",
    ] {
        assert!(
            top.contains(cell),
            "{DOC}'s provenance table (before the first `##` heading) must carry \
             {cell:?}; top of doc:\n{top}"
        );
    }
}

// ── the temp-dir layout and the orphan-sweep premise ────────────────────────

#[test]
fn temp_dir_layout_and_pid_premise_match_the_documented_contract() {
    let _guard = global_lock();
    let installer = HookInstaller::new().expect("HookInstaller::new");
    let dir = installer.dir_path();

    // `<TMPDIR>/claude-print-<pid>-<rand>/`: the dir lands in the temp dir
    // std actually resolves, and its name embeds THIS process's live pid —
    // the premise `cleanup_orphans`'s `kill(pid, 0)` liveness check rests on.
    assert_eq!(
        dir.parent(),
        Some(std::env::temp_dir().as_ref()),
        "the per-run dir must live directly under <TMPDIR>"
    );
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_else(|| panic!("temp dir name must be UTF-8: {}", dir.display()));
    let rest = name
        .strip_prefix("claude-print-")
        .unwrap_or_else(|| panic!("temp dir name must start `claude-print-`: {name}"));
    let (pid, rand) = rest
        .split_once('-')
        .unwrap_or_else(|| panic!("temp dir name must be <pid>-<rand>: {name}"));
    assert_eq!(
        pid,
        std::process::id().to_string(),
        "the pid embedded in the dir name must be the owning process's"
    );
    assert!(!rand.is_empty(), "the <rand> segment must be non-empty");

    // The documented tree: four installer-created artifacts, and
    // session-identity.json deliberately absent until identity.sh writes it.
    for file in ["settings.json", "hook.sh", "identity.sh"] {
        assert!(
            dir.join(file).is_file(),
            "the installer must create {file} — the documented tree"
        );
    }
    assert!(
        installer.fifo_path.exists(),
        "the installer must create stop.fifo — the documented tree"
    );
    assert!(
        fs::metadata(&installer.fifo_path)
            .expect("stat stop.fifo")
            .file_type()
            .is_fifo(),
        "stop.fifo must be a named pipe"
    );
    assert_eq!(
        installer.fifo_path,
        dir.join("stop.fifo"),
        "stop.fifo must sit directly in the per-run dir"
    );
    assert!(
        !installer.identity_path.exists(),
        "session-identity.json is written at runtime by identity.sh, never \
         created by the installer"
    );

    // The sweep premise is wired where the doc says: every invocation
    // begins with the orphan sweep.
    let main_rs = fs::read_to_string(repo_root().join("src/main.rs"))
        .expect("read src/main.rs for the sweep wiring");
    assert!(
        main_rs.contains("cleanup_orphans"),
        "src/main.rs must call hook::cleanup_orphans() at startup — the doc's \
         orphan-sweep premise"
    );

    // The doc publishes all of the above.
    let doc = normalized(&read_doc());
    for phrase in [
        "<TMPDIR>/claude-print-<pid>-<rand>/",
        "The owning pid embedded in the directory name (`claude-print-<pid>-<rand>`) \
         is load-bearing: the startup orphan sweep uses it to prove a stale \
         directory's owner is dead before reclaiming it",
        "session-identity.json # Written at runtime by identity.sh (not created \
         by the installer)",
        "every invocation begins with `hook::cleanup_orphans()` at the top of `main()`",
    ] {
        assert!(
            doc.contains(normalized(phrase).as_str()),
            "{DOC} must publish {phrase:?}"
        );
    }
}

// ── the artifact-mode guarantees ─────────────────────────────────────────────

#[test]
fn relay_artifact_modes_match_the_documented_guarantees() {
    let _guard = global_lock();
    let installer = HookInstaller::new().expect("HookInstaller::new");
    assert_eq!(
        mode_of(installer.dir_path()),
        0o700,
        "the per-run dir must be owner-only 0700"
    );
    assert_eq!(
        mode_of(&installer.fifo_path),
        0o600,
        "stop.fifo must be owner-rw 0600"
    );
    for script in ["hook.sh", "identity.sh"] {
        assert_eq!(
            mode_of(&installer.dir_path().join(script)),
            0o750,
            "{script} must be executable 0750"
        );
    }

    // The doc publishes the three modes where it promises them.
    let doc = read_doc();
    let temp = section(&doc, "Temp Directory Structure");
    for mode in ["0700", "0600"] {
        assert!(
            temp.contains(mode),
            "§Temp Directory Structure must publish the {mode} guarantee"
        );
    }
    assert!(
        section(&doc, "Relay Hook").contains("0750"),
        "§Relay Hook must publish the 0750 relay-script mode"
    );
}

/// The doc's hostile case: a umask of 000 must not loosen any pinned mode
/// (an unpinned mkdir would land the dir world-writable). The flip is
/// process-wide but held only across `HookInstaller::new`, mirroring
/// `src/hook.rs::artifact_modes_hold_under_hostile_umask`; every mode the
/// doc promises is chmod-pinned, so concurrent constructions are unaffected.
#[test]
fn relay_modes_hold_under_the_documented_hostile_umask() {
    let _guard = global_lock();
    let previous_mask = unsafe { libc::umask(0o000) };
    let installer = HookInstaller::new().expect("HookInstaller::new under umask 000");
    unsafe { libc::umask(previous_mask) };

    assert_eq!(mode_of(installer.dir_path()), 0o700);
    assert_eq!(mode_of(&installer.fifo_path), 0o600);
    for script in ["hook.sh", "identity.sh"] {
        assert_eq!(mode_of(&installer.dir_path().join(script)), 0o750);
    }

    assert!(
        section(&read_doc(), "Temp Directory Structure").contains("hostile umask 000"),
        "§Temp Directory Structure must keep pinning the hostile-umask-000 case"
    );
}

// ── the relay-script body and its escaping ───────────────────────────────────

/// The single-quoted target of a `cat > '<target>' 2>/dev/null || true`
/// script body, still in its escaped form.
fn quoted_target(script: &str) -> &str {
    let start = script
        .find("cat > '")
        .expect("relay script must carry the `cat > '` prefix")
        + "cat > '".len();
    let end = start
        + script[start..]
            .find("' 2>/dev/null")
            .expect("relay script must close the quote before `2>/dev/null`");
    &script[start..end]
}

/// Both relay scripts are byte-exactly the documented body: the shebang,
/// the single-quoted target (no variable expansion at execution time), the
/// silenced stderr, and the `|| true` that lets a failed write exit
/// cleanly. Any change to `write_cat_script` fails here until the doc
/// moves with it.
#[test]
fn relay_script_bodies_are_the_documented_cat_redirect_or_true() {
    let _guard = global_lock();
    let installer = HookInstaller::new().expect("HookInstaller::new");
    let identity_script = installer.dir_path().join("identity.sh");
    for (script, target) in [
        (installer.hook_path.as_path(), installer.fifo_path.as_path()),
        (identity_script.as_path(), installer.identity_path.as_path()),
    ] {
        let body = fs::read_to_string(script).unwrap_or_else(|e| {
            panic!("read {}: {e}", script.display());
        });
        assert_eq!(
            body,
            format!(
                "#!/bin/sh\ncat > '{}' 2>/dev/null || true\n",
                target.to_string_lossy()
            ),
            "{} must be the documented relay body byte-for-byte",
            script.display()
        );
    }

    // The doc publishes the body, the failure-swallowing clause, and the
    // shared generator.
    let relay = normalized(section(&read_doc(), "Relay Hook"));
    for phrase in [
        "#!/bin/sh cat > '<fifo-path>' 2>/dev/null || true",
        "The identity relay (`identity.sh`) is the same script with \
         `<identity-path>` in place of `<fifo-path>`.",
        "The target path is embedded as a shell single-quoted string",
        "If the write fails, the hook exits cleanly (`|| true`)",
        "written by the same generator (`write_cat_script`)",
    ] {
        assert!(
            relay.contains(normalized(phrase).as_str()),
            "§Relay Hook must publish {phrase:?}"
        );
    }
}

/// The documented `'\''` escape, replayed for real: TMPDIR is pointed at a
/// directory whose path contains a single quote, the installer is built
/// there, and the generated scripts must carry the escape and unescape
/// back to the exact target paths.
#[test]
fn relay_scripts_escape_single_quotes_in_the_temp_dir_path() {
    let _guard = global_lock();
    let outer = tempfile::tempdir().expect("outer tempdir");
    let quoted_tmp = outer.path().join("doc-'pin");
    fs::create_dir(&quoted_tmp).expect("create the quote-carrying TMPDIR");

    let previous_tmpdir = std::env::var_os("TMPDIR");
    std::env::set_var("TMPDIR", &quoted_tmp);
    let installer = HookInstaller::new().expect("HookInstaller::new under a quoted TMPDIR");
    match previous_tmpdir {
        Some(value) => std::env::set_var("TMPDIR", value),
        None => std::env::remove_var("TMPDIR"),
    }

    let identity_script = installer.dir_path().join("identity.sh");
    for (script, target) in [
        (installer.hook_path.as_path(), installer.fifo_path.as_path()),
        (identity_script.as_path(), installer.identity_path.as_path()),
    ] {
        let body =
            fs::read_to_string(script).unwrap_or_else(|e| panic!("read {}: {e}", script.display()));
        let escaped = quoted_target(&body);
        assert!(
            escaped.contains("'\\''"),
            "a single quote in the temp-dir path must be escaped `'\\''` in {}; \
             escaped target: {escaped:?}",
            script.display()
        );
        // Round-trip: undoing the escape must reconstruct the real path —
        // the property that makes metacharacters in the path inert.
        assert_eq!(
            escaped.replace("'\\''", "'"),
            target.to_string_lossy(),
            "the escaped target in {} must unescape to the real path",
            script.display()
        );
    }

    assert!(
        normalized(section(&read_doc(), "Relay Hook")).contains("escaped `'\\''`"),
        "§Relay Hook must publish the `'\\''` escape"
    );
}

// ── the session-identity sibling invariant ───────────────────────────────────

/// `session-identity.json` is the sibling of `stop.fifo` by contract:
/// pool clients reconstruct its path from the Stop FIFO path alone, so the
/// reconstruction must land exactly on the installer's identity path.
#[test]
fn session_identity_is_the_documented_stop_fifo_sibling() {
    let _guard = global_lock();
    let installer = HookInstaller::new().expect("HookInstaller::new");
    assert_eq!(
        installer.identity_path.file_name().and_then(|n| n.to_str()),
        Some(SESSION_IDENTITY_FILE),
        "the identity file must be named SESSION_IDENTITY_FILE"
    );
    assert_eq!(
        installer.fifo_path.with_file_name(SESSION_IDENTITY_FILE),
        installer.identity_path,
        "the pool-client reconstruction stop_fifo().with_file_name(\
         SESSION_IDENTITY_FILE) must land on the installer's identity path"
    );

    let doc = normalized(&read_doc());
    for phrase in [
        "pool clients reconstruct its path from the Stop FIFO path alone \
         (`stop_fifo().with_file_name(SESSION_IDENTITY_FILE)`)",
        "the sibling layout is part of the daemon/client contract",
        "pool clients reconstruct it as `stop_fifo().with_file_name(\
         SESSION_IDENTITY_FILE)` because the assignment frame carries only \
         the Stop FIFO path",
    ] {
        assert!(
            doc.contains(normalized(phrase).as_str()),
            "{DOC} must publish {phrase:?}"
        );
    }
}

// ── the per-run settings.json hook wiring ────────────────────────────────────

/// The settings file carries exactly the two documented relay hooks — Stop
/// → hook.sh and UserPromptSubmit → identity.sh, both `type: command` with
/// `timeout: 10` — and the child argv wiring forwards it as
/// `--settings=<temp>/settings.json` from both spawn sites.
#[test]
fn settings_json_carries_the_documented_two_hook_wiring() {
    let _guard = global_lock();
    let installer = HookInstaller::new().expect("HookInstaller::new");
    assert_eq!(
        installer.settings_path,
        installer.dir_path().join("settings.json"),
        "settings.json must sit directly in the per-run dir"
    );
    let parsed: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&installer.settings_path)
            .unwrap_or_else(|e| panic!("read settings.json: {e}")),
    )
    .expect("settings.json must parse");
    let hooks = parsed
        .get("hooks")
        .and_then(|h| h.as_object())
        .unwrap_or_else(|| panic!("settings.json must carry a hooks object"));

    // Closed world: exactly the two relay events, nothing else.
    let events: BTreeSet<&str> = hooks.keys().map(String::as_str).collect();
    assert_eq!(
        events,
        BTreeSet::from(["Stop", "UserPromptSubmit"]),
        "settings.json must wire exactly the Stop and UserPromptSubmit relays"
    );

    let identity_script = installer.dir_path().join("identity.sh");
    for (event, command) in [
        ("Stop", installer.hook_path.as_path()),
        ("UserPromptSubmit", identity_script.as_path()),
    ] {
        let entry = &hooks[event][0]["hooks"][0];
        assert_eq!(
            entry["type"], "command",
            "{event} relay must be a command hook"
        );
        assert_eq!(
            entry["command"].as_str(),
            Some(command.to_string_lossy().as_ref()),
            "{event} relay must point at the generated script"
        );
        assert_eq!(entry["timeout"], 10, "{event} relay must keep timeout 10");
    }

    // The doc publishes the schema and the wiring sentence.
    let doc = read_doc();
    let settings = normalized(section(&doc, "settings.json"));
    for phrase in [
        "\"Stop\": [{",
        "\"UserPromptSubmit\": [{",
        "\"command\": \"<temp>/hook.sh\", \"timeout\": 10",
        "\"command\": \"<temp>/identity.sh\", \"timeout\": 10",
    ] {
        assert!(
            settings.contains(normalized(phrase).as_str()),
            "§settings.json must publish {phrase:?}"
        );
    }
    assert!(
        normalized(section(&doc, "Relay Hook"))
            .contains("executed by Claude Code via `--settings <temp>/settings.json`"),
        "§Relay Hook must publish the --settings wiring"
    );

    // And the wiring is real: both spawn sites forward the per-run file.
    for src in ["src/session.rs", "src/pool.rs"] {
        let content =
            fs::read_to_string(repo_root().join(src)).unwrap_or_else(|e| panic!("read {src}: {e}"));
        assert!(
            content.contains("\"--settings={}\""),
            "{src} must forward --settings=<temp>/settings.json to the child"
        );
    }
}

// The repo-root resolution itself: pinned so a future edit can't quietly
// reintroduce a baked-only root — the failure mode the candidate chain
// exists for (a close gate re-running this suite in a fresh extraction of
// unchanged content instant-reuses the cached binary, and a baked-only
// root fails every filesystem test there with FileNotFound, which reads
// as drift but is cache state; bead claudepr-270570be).

#[test]
fn repo_root_resolution_follows_the_candidate_chain() {
    // The live checkout the suite is running in — the same chain the
    // suite's repo-root resolution uses, minus the override.
    let live = resolve_repo_root(
        None,
        std::env::var("CARGO_MANIFEST_DIR").ok().as_deref(),
        env!("CARGO_MANIFEST_DIR"),
    )
    .expect("the running suite has a usable repo root");
    let live_str = live.display().to_string();
    // A second, minimal checkout: resolution only stats the probe files, so
    // empty ones are enough to make it a valid candidate.
    let other = tempfile::tempdir().expect("tempdir for a second repo root");
    for probe in ROOT_PROBES {
        std::fs::write(other.path().join(probe), "").expect("writing root probe file");
    }
    let other_str = other.path().display().to_string();

    // 1. the override outranks the runtime manifest when both are checkouts
    assert_eq!(
        resolve_repo_root(Some(&other_str), Some(&live_str), &live_str),
        Ok(other.path().to_path_buf())
    );
    // 2. the runtime manifest outranks the baked value — the cache-reuse
    //    case: a dead baked path loses to the live extraction
    assert_eq!(
        resolve_repo_root(None, Some(&other_str), &live_str),
        Ok(other.path().to_path_buf())
    );
    // 3. the baked value is the fallback (direct binary runs: cargo sets
    //    no runtime manifest)
    assert_eq!(
        resolve_repo_root(None, None, &other_str),
        Ok(other.path().to_path_buf())
    );
}

#[test]
fn repo_root_resolution_fails_loudly_naming_every_candidate() {
    // An existing directory without the probe files — the shape a deleted
    // extraction's path, or a typo'd path, has.
    let not_a_checkout = tempfile::tempdir().expect("tempdir that is not a checkout");
    let bogus = not_a_checkout.path().display().to_string();
    let err = resolve_repo_root(None, Some(&bogus), &bogus).unwrap_err();
    assert!(
        err.contains(&bogus),
        "the failure must name the rejected candidate: {err}"
    );
    assert!(
        err.contains("CLAUDE_PRINT_TEST_REPO"),
        "the failure must name the escape hatch: {err}"
    );
    assert!(
        err.contains(ROOT_PROBES[0]) && err.contains(ROOT_PROBES[1]),
        "the failure must name the probe files so the gap is actionable: {err}"
    );
}

#[test]
fn a_set_repo_root_override_is_authoritative() {
    let live = resolve_repo_root(
        None,
        std::env::var("CARGO_MANIFEST_DIR").ok().as_deref(),
        env!("CARGO_MANIFEST_DIR"),
    )
    .expect("the running suite has a usable repo root");
    let live_str = live.display().to_string();
    let not_a_checkout = tempfile::tempdir().expect("tempdir that is not a checkout");
    let bogus = not_a_checkout.path().display().to_string();
    let err = resolve_repo_root(Some(&bogus), Some(&live_str), &live_str).unwrap_err();
    assert!(
        err.contains("$CLAUDE_PRINT_TEST_REPO") && err.contains(&bogus),
        "a set-but-wrong override must fail naming itself, not fall through to \
         another tree: {err}"
    );
}
