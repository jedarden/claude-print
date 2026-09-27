//! Behavioral contract for the documented `cargo metadata` target-dir
//! lookup (bead claudepr-307fc854).
//!
//! AGENTS.md §"Where the build output lands" and README's build-from-source
//! section both resolve build output through a one-line extraction of
//! `cargo metadata`'s `target_directory`. The line was originally spelled
//! with `jq` — a tool no prerequisite section names and no stock Cargo
//! environment guarantees — so the docs now spell it with plain POSIX
//! `sed`, the same jq-free extractor `scripts/check-billing.sh` already
//! falls back to. `tests/docs_build_layout.rs` pins AGENTS.md's copy
//! verbatim as documentation; this suite pins the lookup to *reality*:
//!
//! - **The documented extraction resolves the real target dir.** The exact
//!   documented line runs under `sh` against the live `cargo`, and the
//!   value it captures is the target dir the running test binary was
//!   actually built into (derived from its own location the way AGENTS.md
//!   documents for the suites) — under the fleet wrapper's redirect and a
//!   stock checkout alike, because `cargo metadata` sees the same
//!   `CARGO_TARGET_DIR` the build did.
//! - **The address the snippet's second line builds from the lookup
//!   resolves to a real executable** — proven with a `[ -x ]` probe on the
//!   composite `$TARGET/<profile>/…` path, not by executing the binary:
//!   whether `--check` succeeds is an environment fact the docs
//!   deliberately leave unpinned (the docs_build_layout boundary).
//! - **Failure modes fail empty, then loudly.** A payload without the key
//!   extracts to the empty string, a `cargo metadata` that fails (no
//!   manifest up-tree) captures the empty string, and the documented
//!   second line then exits non-zero on the resulting path — never a
//!   silently wrong directory, which is the property the docs claim.
//! - **Both docs carry the replacement verbatim and only the replacement**
//!   — the jq spelling may not return alongside it (AGENTS.md's fence is
//!   additionally pinned by `tests/docs_build_layout.rs`; README's has no
//!   other guard).
//!
//! Compiled-binaries like the suites whose locator derivation it mirrors:
//! resolves the test binary through `current_exe()` and spawns `sh`
//! children that run the real `cargo` and `sed` the lookup names.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Probe files that identify a claude-print checkout: a directory holding
/// both is a usable repo root for this guard.
const ROOT_PROBES: [&str; 2] = ["AGENTS.md", "Cargo.toml"];

/// Repository root this suite reads, resolved at *runtime* — never the bare
/// compile-time `env!("CARGO_MANIFEST_DIR")`, which bakes the building
/// checkout's path into the test binary. The local cargo wrapper maps
/// `.git`-less extractions onto one shared target dir, so an extraction of
/// unchanged content instant-reuses a cached test binary compiled in an
/// extraction that has since been deleted; a baked-only root then fails
/// every later run of that binary with file-NotFound panics that have
/// nothing to do with drift (bead claudepr-23f81f16). Candidates, most
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
/// If no candidate survives its probe the suite panics naming every
/// candidate it rejected — loud, never a vacuous pass off a wrong tree.
fn repo_root() -> PathBuf {
    resolve_repo_root(
        std::env::var("CLAUDE_PRINT_TEST_REPO").ok().as_deref(),
        std::env::var("CARGO_MANIFEST_DIR").ok().as_deref(),
        env!("CARGO_MANIFEST_DIR"),
    )
    .unwrap_or_else(|e| panic!("locating the repo root to read AGENTS.md and README.md from: {e}"))
}

/// [`repo_root`]'s candidate chain as a pure function, so the precedence
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

/// The documented lookup line, byte-exact as both docs spell it: cargo
/// reports the target dir, POSIX `sed` extracts the field. No `jq` — no
/// prerequisite section names it and no stock Cargo environment guarantees
/// it, which is why the lookup was deliberately freed from that spelling.
const LOOKUP_LINE: &str = r#"TARGET="$(cargo metadata --no-deps --format-version 1 | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')""#;

/// The extraction program alone, for feeding payloads that never came from
/// cargo.
const SED_PROGRAM: &str = r#"sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p'"#;

/// The artifact address AGENTS.md's fence builds from the lookup — the
/// artifact table's stock debug row.
const AGENTS_ARTIFACT_LINE: &str = "\"$TARGET/debug/claude-print\" --check";

/// The artifact address README's fence builds from the lookup.
const README_ARTIFACT_LINE: &str = "\"$TARGET/release/claude-print\" --version";

/// The jq spelling the lookup replaced. Its return, in either doc, is
/// drift by name.
const JQ_SPELLING: &str = "jq -r .target_directory";

/// Runs `script` under `sh` in `dir`, returning `(stdout, success, stderr)`.
fn sh_in(dir: &Path, script: &str) -> (String, bool, String) {
    let out = Command::new("sh")
        .arg("-c")
        .arg(script)
        .current_dir(dir)
        .output()
        .expect("spawning sh — the lookup names nothing but sh-portable tools");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The target dir and profile this test binary was built into, derived from
/// `current_exe()` the way AGENTS.md documents for the suites: the binary
/// sits in <target>/<profile>/deps/, so the profile dir is one parent up
/// and the target dir two.
fn target_and_profile_of_this_test() -> (PathBuf, String) {
    let exe = std::env::current_exe().expect("current_exe");
    let deps = exe.parent().expect("the test binary has a parent dir");
    assert_eq!(
        deps.file_name().and_then(|n| n.to_str()),
        Some("deps"),
        "the documented current_exe() resolution assumes test binaries live \
         under <target>/<profile>/deps/ — found this one at {}",
        exe.display()
    );
    let profile_dir = deps.parent().expect("deps/ has a parent");
    let profile = profile_dir
        .file_name()
        .and_then(|n| n.to_str())
        .expect("profile dir has a name")
        .to_string();
    assert!(
        ["debug", "release"].contains(&profile.as_str()),
        "the profile dir {profile:?} matches no documented artifact shape — \
         the fences address debug and release layouts only"
    );
    let target_dir = profile_dir.parent().expect("profile dir has a parent");
    (target_dir.to_path_buf(), profile)
}

#[test]
fn the_documented_lookup_resolves_the_real_target_directory() {
    let root = repo_root();
    let (target_dir, _) = target_and_profile_of_this_test();
    let script = format!("{LOOKUP_LINE}\nprintf '%s' \"$TARGET\"");
    let (stdout, ok, stderr) = sh_in(&root, &script);
    assert!(
        ok,
        "sh itself failed running the documented lookup: {stderr}"
    );
    assert!(
        !stdout.is_empty(),
        "the documented lookup captured nothing in a checkout where cargo \
         metadata succeeds — the extraction is broken against real output"
    );
    assert_eq!(
        fs::canonicalize(&stdout).unwrap_or_else(|e| {
            panic!(
                "the documented lookup printed {stdout:?}, which does not \
                 resolve: {e}"
            )
        }),
        fs::canonicalize(&target_dir).expect("the target dir this test binary ran from exists"),
        "the documented extraction must resolve the same target dir the test \
         binary was actually built into — the lookup is cargo-derived under \
         the fleet wrapper's redirect and a stock checkout alike"
    );
}

#[test]
fn the_address_the_documented_second_line_builds_resolves_to_a_real_executable() {
    let root = repo_root();
    let (_, profile) = target_and_profile_of_this_test();
    // The full documented pair, with the second line's existence claim made
    // checkable hermetically: `[ -x ]` proves the composite path resolves
    // to an executable without running the binary (the environment half of
    // the claim, deliberately unpinned).
    let script = format!("{LOOKUP_LINE}\n[ -x \"$TARGET/{profile}/claude-print\" ]");
    let (_, ok, stderr) = sh_in(&root, &script);
    assert!(
        ok,
        "the address the documented second line builds from the lookup did \
         not resolve to an executable for the {profile} profile this suite \
         was built into: {stderr}"
    );
}

#[test]
fn metadata_without_the_key_extracts_to_the_empty_string() {
    let tmp = tempfile::tempdir().expect("tempdir for the payload legs");
    // Both shapes of a payload without the field: cargo failing leaves only
    // stderr (empty stdout), and a JSON object without the key — either way
    // the docs promise TARGET comes back empty, never a wrong directory.
    for payload in ["", "{\"packages\":[],\"metadata\":null}"] {
        let script = format!(
            "TARGET=\"$(printf '%s' '{payload}' | {SED_PROGRAM})\"\nprintf '%s' \"$TARGET\""
        );
        let (stdout, ok, stderr) = sh_in(tmp.path(), &script);
        assert!(ok, "sh failed on the payload leg: {stderr}");
        assert!(
            stdout.is_empty(),
            "a payload without target_directory extracted {stdout:?} — the \
             documented failure mode is an empty TARGET, not a partial one"
        );
    }
}

#[test]
fn a_failed_lookup_fails_the_next_line_instead_of_addressing_a_wrong_directory() {
    // No manifest up-tree from an empty temp dir: cargo metadata fails,
    // TARGET comes back empty, and the documented second line then fails on
    // the resulting path — loudly, which is the docs' claim for the whole
    // failure mode.
    let tmp = tempfile::tempdir().expect("tempdir without a manifest");
    let script = format!("{LOOKUP_LINE}\n{AGENTS_ARTIFACT_LINE}");
    let (stdout, ok, stderr) = sh_in(tmp.path(), &script);
    assert!(
        !ok,
        "the second documented line succeeded off a failed lookup — it must \
         fail on the empty-target path, never silently address a directory"
    );
    assert!(
        stdout.is_empty(),
        "the failed pair printed {stdout:?} — a failed lookup has no output"
    );
    assert!(
        stderr.contains("claude-print"),
        "sh's failure must name the path it could not run — the docs' \
         \"fails on the resulting path\" claim: {stderr}"
    );
}

#[test]
fn both_docs_carry_the_jq_free_lookup_verbatim() {
    let root = repo_root();
    let agents = fs::read_to_string(root.join("AGENTS.md")).expect("reading AGENTS.md");
    let readme = fs::read_to_string(root.join("README.md")).expect("reading README.md");
    for (doc, name, artifact) in [
        (&agents, "AGENTS.md", AGENTS_ARTIFACT_LINE),
        (&readme, "README.md", README_ARTIFACT_LINE),
    ] {
        let fence_body = format!("{LOOKUP_LINE}\n{artifact}");
        assert!(
            doc.contains(&fence_body),
            "{name} lost the documented lookup fence:\n\n{fence_body}\n\n— \
             the lookup is the only documented way to resolve build output \
             without hardcoding a layout"
        );
        assert!(
            !doc.contains(JQ_SPELLING),
            "{name} spells the target-dir lookup with {JQ_SPELLING:?} again — \
             the lookup is POSIX `sed` precisely because no stock Cargo \
             environment guarantees `jq`; fix the drift, don't pin it"
        );
    }
}
