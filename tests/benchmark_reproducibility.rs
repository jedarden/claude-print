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
use std::path::PathBuf;

use serde_json::Value;

const ARTIFACT: &str = "docs/notes/startup-overhead-benchmark.json";
const NOTE: &str = "docs/notes/startup-overhead-benchmark.md";
const SCHEMA_V2: &str = "claude-print/startup-overhead-benchmark/2";

/// Repo-rooted path, resolved from the *runtime* `CARGO_MANIFEST_DIR` with
/// the compile-time value as fallback — the same resolution
/// `tests/docs_slug_consistency.rs` uses, so a test binary reused from the
/// shared target cache by a different extraction still reads the tree under
/// test rather than a baked-in path that may no longer exist.
fn repo_path(rel: &str) -> PathBuf {
    let root = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string()),
    );
    root.join(rel)
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
