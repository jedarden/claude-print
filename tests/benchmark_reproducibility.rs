//! Reproducibility guard for the committed startup-overhead benchmark
//! evidence (bead claudepr-70a60152).
//!
//! The schema-1 recording of the 2026-09-19 run carried
//! `harness.bin_dir`, `harness.claude_print_path`, and
//! `harness.mock_claude_path` set to `/build/target-workers/release` — the
//! fleet host's redirected cargo target dir *at the time*. That path was
//! machine-specific twice over: no other box could resolve it, and the
//! per-repo redirect that later replaced it (`/build/claude-print`, AGENTS.md
//! "Where the build output lands") means even the measuring host no longer
//! produces it. Repository guidance is to derive the target dir through
//! `cargo metadata`, never hardcode either location — the committed evidence
//! and its reproduce instructions have to follow the same rule.
//!
//! Schema 2 fixes both halves: `scripts/bench_startup_overhead.py` derives
//! the bin dir from `cargo metadata` when `--bin-dir` is omitted (with
//! `--profile` selecting the build profile), records only the derivation
//! (`harness.bin_dir_source`), and redacts an explicit `--bin-dir` from the
//! recorded argv. This test pins that shape so the drift cannot silently
//! return.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const ARTIFACT: &str = "docs/notes/startup-overhead-benchmark.json";
const NOTE: &str = "docs/notes/startup-overhead-benchmark.md";
const SCHEMA_V2: &str = "claude-print/startup-overhead-benchmark/2";

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

fn read(rel: &str) -> String {
    fs::read_to_string(repo_path(rel))
        .unwrap_or_else(|e| panic!("read {rel} from the repo root: {e}"))
}

/// Collect every string value in a JSON subtree (objects and arrays
/// descended into), for the "no recorded absolute paths" sweep.
fn string_values(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|v| string_values(v, out)),
        Value::Object(map) => map.values().for_each(|v| string_values(v, out)),
        _ => {}
    }
}

#[test]
fn committed_artifact_carries_no_machine_specific_paths() {
    let raw = read(ARTIFACT);
    for fragment in ["/build/", "target-workers"] {
        assert!(
            !raw.contains(fragment),
            "committed benchmark artifact contains the machine-specific path \
             fragment {fragment:?} — record the derivation (schema 2's \
             harness.bin_dir_source), not a host absolute path"
        );
    }
}

#[test]
fn committed_artifact_harness_block_records_the_derivation() {
    let json: Value =
        serde_json::from_str(&read(ARTIFACT)).expect("committed benchmark artifact parses as JSON");
    assert_eq!(
        json["schema"], SCHEMA_V2,
        "artifact schema drifted — update the harness ARTIFACT_SCHEMA \
         constant, this note, README, and docs/plan/plan.md together"
    );

    let harness = &json["harness"];
    for legacy_key in ["bin_dir", "claude_print_path", "mock_claude_path"] {
        assert!(
            harness[legacy_key].is_null(),
            "harness block still carries the schema-1 absolute-path key \
             {legacy_key:?} — schema 2 records bin_dir_source instead"
        );
    }

    let source = harness["bin_dir_source"]
        .as_str()
        .expect("harness.bin_dir_source must be a string")
        .trim()
        .to_owned();
    assert!(
        !source.is_empty(),
        "harness.bin_dir_source is empty — the derivation is the evidence"
    );

    let argv = harness["argv"]
        .as_array()
        .expect("harness.argv must be an array");
    assert!(
        !argv.is_empty(),
        "harness.argv lost the recorded invocation"
    );

    // No string anywhere in the harness block may be an absolute path:
    // relative script/output paths are portable, host filesystem layout is
    // not (this is what catches a redaction regression or a new key
    // reintroducing a resolved path).
    let mut strings = Vec::new();
    string_values(harness, &mut strings);
    assert!(
        strings.iter().any(|s| s.contains("bench_startup_overhead")),
        "harness block no longer names the harness — check that the \
         recorded argv is intact"
    );
    for s in &strings {
        assert!(
            !s.starts_with('/'),
            "harness block records the absolute path {s:?} — host layout \
             does not belong in committed evidence"
        );
    }
}

#[test]
fn benchmark_note_derives_the_bin_dir_instead_of_hardcoding_it() {
    let note = read(NOTE);
    // Guard the guard: the note must still carry the reproduce command and
    // the schema reference, or the checks above pass vacuously after a
    // section deletion.
    assert!(
        note.contains("bench_startup_overhead.py"),
        "the reproduce command disappeared from the benchmark note"
    );
    assert!(
        note.contains(SCHEMA_V2),
        "benchmark note's schema reference drifted from the artifact's"
    );
    assert!(
        !note.contains("--bin-dir /"),
        "benchmark note hardcodes an absolute --bin-dir argument — derive it \
         via cargo metadata / --profile instead"
    );
    assert!(
        note.contains("cargo metadata"),
        "benchmark note must show the cargo-metadata bin-dir derivation that \
         AGENTS.md mandates"
    );
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
