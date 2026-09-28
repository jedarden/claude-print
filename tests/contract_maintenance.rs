//! Contract-probe maintenance wiring guard (bead claudepr-e8fc5744).
//!
//! `docs/notes/claude-contract-probes.md` §Maintenance defines the maintenance
//! step (detect → re-run → re-pin → file follow-ups) and
//! `scripts/contract-maintenance-gate.sh` is its executable owner, wired into
//! the `claude-print-ci` WorkflowTemplate so it runs on every push. These
//! tests pin that wiring so the automation cannot silently detach from the
//! doc again:
//!
//! - active-version consistency (claudepr-b590e46d, always-on) — the doc's
//!   **Measured against:** stamp and BOTH active fixture families
//!   (`claude_contracts_v*.json`, `stream_json_golden_v*.{input,expected,
//!   errors}.jsonl`) must name one Claude version; fixture files no test
//!   references are historical captures and are exempt;
//! - doc-prose one-pin (claudepr-893bfc4e, always-on + detector) — the doc's
//!   evidence prose (the stamp, markdown table rows, `Evidence (`
//!   preambles) may cite only the active pin or explicitly `historical`
//!   numbers, so the fixtures-move/prose-lags shape of an incomplete re-pin
//!   (the reverted claudepr-2e8c3884) fails every `cargo test` run and the
//!   CI gate, not just review;
//! - detector parse — `scripts/check-claude-version-bump.sh` exits 0/1/2
//!   against a stubbed `claude`, with the stub's version derived from the
//!   doc's live **Measured against:** stamp, and rejects divergent active
//!   pins against a skeleton repo without consulting any `claude` at all —
//!   as do a family disagreeing with itself (a half-moved golden triple)
//!   and a referenced fixture family missing from tests/fixtures/
//!   (claudepr-b3ac3625);
//! - the gate's contract against a stubbed `claude`/`gh`/`cargo` (hermetic —
//!   a single stub-bin PATH, so unstubbed binaries are genuinely absent):
//!   exit code, evidence-bundle shape, the refreshed version artifact (an
//!   explicit `--version-file`, and the default resolution it shares with
//!   `tests/version_compat.rs` across both cargo layouts), and
//!   `--file-follow-up` idempotency (marker search before issue create);
//! - real-environment self-consistency — the same path CI exercises, with no
//!   PATH override: the gate's verdict mirrors the detector's, and the
//!   version file matches the real `claude --version` (or records `unknown`);
//! - wiring fragments — the WorkflowTemplate invokes the gate on every push
//!   and stamps the status into release notes, the push trigger that
//!   submits that template stays attached (the Sensor's push +
//!   refs/heads/main filters and its workflowTemplateRef hand-off, fed by
//!   the EventSource stanza's push subscription — claudepr-b3ac3625), and
//!   the maintenance doc / plan R-2 / README / AGENTS.md still name it;
//! - gate execution (claudepr-3f4aefad) — the same WorkflowTemplate pinned
//!   comment- and token-aware, because a *commented-out* gate block
//!   satisfies every raw-text fragment above: the invocation must run on
//!   an uncommented line inside the fatal `if ! … exit 1 … fi` wrapper,
//!   before every quality gate and the verify-only green exit, after the
//!   claude install, on top of `set -ex`, with negative meta-tests
//!   planting each drift shape in memory.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

/// The version the contract evidence is currently pinned to, parsed from the
/// maintenance doc the same way `scripts/check-claude-version-bump.sh` does
/// (first x.y.z token of the **Measured against:** line).
fn doc_pin() -> String {
    let doc = fs::read_to_string(repo_path("docs/notes/claude-contract-probes.md"))
        .expect("docs/notes/claude-contract-probes.md must exist");
    let line = doc
        .lines()
        .find(|l| l.starts_with("**Measured against:**"))
        .expect("the doc must carry a **Measured against:** stamp");
    line.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter(|t| !t.is_empty())
        .find(|t| {
            t.split('.').count() == 3
                && t.split('.')
                    .all(|p| !p.is_empty() && p.chars().all(|d| d.is_ascii_digit()))
        })
        .expect("the stamp must contain an x.y.z version")
        .to_string()
}

/// A version that can never equal the pin: patch component +1.
fn bumped(pin: &str) -> String {
    let mut parts: Vec<u32> = pin.split('.').map(|p| p.parse().unwrap()).collect();
    *parts.last_mut().unwrap() += 1;
    parts
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(".")
}

/// Symlink the real core utilities the bash scripts need (bash, grep, head,
/// cat, mkdir, dirname, sort, wc, tr, sed) into the stub bin dir, so a
/// single-entry PATH is self-contained. `printf`/`command`/`cd` are bash
/// builtins and need no link. Idempotent: existing links (and stubs) are
/// left alone.
fn link_coreutils(bin: &Path) {
    fs::create_dir_all(bin).unwrap();
    for tool in [
        "bash", "grep", "head", "cat", "mkdir", "dirname", "sort", "wc", "tr", "sed",
    ] {
        let dest = bin.join(tool);
        if dest.symlink_metadata().is_ok() {
            continue;
        }
        let real = std::env::var_os("PATH")
            .and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join(tool))
                    .find(|p| p.exists())
            })
            .unwrap_or_else(|| panic!("{tool} must be on PATH to build the stub bin dir"));
        std::os::unix::fs::symlink(&real, &dest).unwrap();
    }
}

/// The hermetic PATH for gate/detector runs: exactly one stub bin dir (the
/// tests/install_billing_canary.rs pattern). Every binary the scripts reach
/// is either a stub written by the test or a coreutils symlink — so a binary
/// that was not placed there (`claude` in the indeterminate tests, `gh`, the
/// real `cargo`) is genuinely unreachable, on any host layout. That matters
/// here: this repo's dev host is NixOS, whose system profile dir holds both
/// the coreutils and a claude install, so filtering the real PATH could never
/// separate them.
fn stub_path(bin: &Path) -> String {
    link_coreutils(bin);
    bin.display().to_string()
}

fn write_stub(dir: &Path, name: &str, body: &str) {
    fs::create_dir_all(dir).unwrap();
    let stub = dir.join(name);
    fs::write(&stub, format!("#!/usr/bin/env bash\n{body}\n")).unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A `claude` whose `--version` first line is exactly `line`.
fn stub_claude(dir: &Path, line: &str) {
    write_stub(dir, "claude", &format!("printf '%s\\n' {:?}", line));
}

/// A `gh` that records each invocation (`$*`, one line) to `$GH_ARGS_FILE`
/// and answers the three subcommands the gate uses. `GH_LIST_FOUND=1` makes
/// `issue list` report existing issue #42; `GH_CREATE_FAIL=1` fails creation.
fn stub_gh(dir: &Path) {
    write_stub(
        dir,
        "gh",
        r#"set -u
printf '%s\n' "$*" >> "${GH_ARGS_FILE:?GH_ARGS_FILE not set}"
case "$1 $2" in
    'issue list')
        if [ "${GH_LIST_FOUND:-0}" = 1 ]; then
            printf '[{"number":42}]\n'
        else
            printf '[]\n'
        fi
        ;;
    'issue create')
        if [ "${GH_CREATE_FAIL:-0}" = 1 ]; then
            echo 'synthetic gh create failure' >&2
            exit 1
        fi
        echo 'https://github.com/jedarden/claude-print/issues/43'
        ;;
    'issue comment') exit 0 ;;
    *) echo "stub gh: unexpected subcommand: $*" >&2; exit 1 ;;
esac
"#,
    );
}

/// A `cargo` that records its arguments and passes, standing in for the
/// cheap live-contract run (`cargo test --test claude_contracts -- --ignored`).
/// A `metadata` invocation is answered from `$CARGO_METADATA_JSON` instead —
/// that is the target-dir lookup the gate's default version-artifact
/// resolution makes, and it must stay out of the live-test transcript.
fn stub_cargo(dir: &Path) {
    write_stub(
        dir,
        "cargo",
        r#"set -u
if [ "${1:-}" = "metadata" ]; then
    printf '%s\n' "${CARGO_METADATA_JSON:-}"
    exit 0
fi
printf '%s\n' "$*" >> "${CARGO_ARGS_FILE:?CARGO_ARGS_FILE not set}"
echo 'stub cargo: test claude_contracts ... ok'
exit 0
"#,
    );
}

/// Run one of the repo's bash scripts. `bin` = Some(stub dir) switches to the
/// hermetic minimal PATH; None keeps the real environment (CI's path).
fn run_script(
    script: &str,
    bin: Option<&Path>,
    args: &[String],
    envs: &[(&str, String)],
) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg(repo_path(script));
    if let Some(bin) = bin {
        cmd.env("PATH", stub_path(bin));
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd.args(args).output().unwrap()
}

fn gate_args(dir: &Path, skip_live: bool, extra: &[&str]) -> Vec<String> {
    let mut args = vec![
        "--evidence-dir".to_string(),
        dir.join("evidence").display().to_string(),
        "--version-file".to_string(),
        dir.join("last-claude-version.txt").display().to_string(),
    ];
    if skip_live {
        args.push("--skip-live-tests".to_string());
    }
    args.extend(extra.iter().map(|s| s.to_string()));
    args
}

/// Gate args with the version file left to the script's own default
/// resolution — the surface the stock/fleet layout tests below exercise.
/// `--skip-live-tests` always, so the stub cargo is never invoked for the
/// live contracts either and the only cargo call a default-resolution run
/// can make is the metadata lookup under test.
fn gate_args_without_version_file(dir: &Path) -> Vec<String> {
    vec![
        "--evidence-dir".to_string(),
        dir.join("evidence").display().to_string(),
        "--skip-live-tests".to_string(),
    ]
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// One `key: value` line out of contract-status.txt.
fn status_value(evidence: &Path, key: &str) -> String {
    let status = fs::read_to_string(evidence.join("contract-status.txt"))
        .expect("contract-status.txt must exist");
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(&format!("{key}: ")) {
            return rest.trim().to_string();
        }
    }
    panic!("no '{key}:' line in contract-status.txt:\n{status}");
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

// ── Active-version consistency (claudepr-b590e46d; always-on, no claude) ─────

/// The version-pinned fixture classes the maintenance gate treats as active
/// evidence, paired with the contract test that selects each family.
const ACTIVE_FIXTURE_SOURCES: [(&str, &str); 2] = [
    ("claude_contracts", "tests/claude_contracts.rs"),
    ("stream_json_golden", "tests/stream_json_contract.rs"),
];

/// A version-shaped token (`x.y.z`, all-numeric parts) at the start of `s`,
/// or None. A trailing `.` (the fixture-extension boundary, as in
/// `2.1.282.json`) ends the token and is not part of it.
fn leading_version(s: &str) -> Option<&str> {
    let end = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let candidate = s[..end].trim_end_matches('.');
    let parts: Vec<&str> = candidate.split('.').collect();
    (parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())))
    .then_some(candidate)
}

/// Every version-shaped token (`x.y.z`, all-numeric parts) cited on one line,
/// mirroring the detector's `grep -oE '[0-9]+\.[0-9]+\.[0-9]+'` scan: greedy
/// digit runs, leftmost-first, resuming after each match — so this mirror and
/// the shell cannot disagree about what a line "cites".
fn cited_versions(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match version_starting_at(bytes, at) {
            Some(end) => {
                out.push(line[at..end].to_string());
                at = end;
            }
            None => at += 1,
        }
    }
    out
}

/// The exclusive end of an `x.y.z` match starting exactly at `at` (each part
/// a greedy digit run), or `None` — one position's attempt in `grep -oE`'s
/// leftmost scan.
fn version_starting_at(bytes: &[u8], at: usize) -> Option<usize> {
    let digits_end = |mut i: usize| -> usize {
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        i
    };
    let digit_at = |i: usize| bytes.get(i).copied().is_some_and(|b| b.is_ascii_digit());
    if !digit_at(at) {
        return None;
    }
    let a = digits_end(at);
    if bytes.get(a).copied() != Some(b'.') || !digit_at(a + 1) {
        return None;
    }
    let b = digits_end(a + 1);
    if bytes.get(b).copied() != Some(b'.') || !digit_at(b + 1) {
        return None;
    }
    Some(digits_end(b + 1))
}

/// Whether the line carries the historical-attribution marker — the whole
/// word `historical`, any case, word-boundaries as `grep -w` computes them
/// (non-alphanumeric-and-underscore on both sides) — which the detector
/// accepts as the explicit attribution a superseded citation must carry.
fn carries_historical_marker(line: &str) -> bool {
    fn word_byte(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_'
    }
    let hay = line.to_ascii_lowercase();
    let bytes = hay.as_bytes();
    hay.match_indices("historical").any(|(at, _)| {
        (at == 0 || !word_byte(bytes[at - 1]))
            && (at + "historical".len() == bytes.len()
                || !word_byte(bytes[at + "historical".len()]))
    })
}

/// The class of doc line the prose one-pin check examines, mirroring the
/// detector's `case` patterns: the stamp, a markdown table row (leading
/// whitespace then `|`), or an `Evidence (` preamble.
fn prose_line_kind(line: &str) -> Option<&'static str> {
    if line.starts_with("**Measured against:**") {
        Some("the Measured-against stamp")
    } else if line.trim_start().starts_with('|') {
        Some("an evidence-table row")
    } else if line.starts_with("Evidence ") {
        Some("an evidence preamble")
    } else {
        None
    }
}

/// Every active version-pinned fixture reference in a contract test source —
/// `(class, version)` pairs parsed from `fixtures/<class>_v<x.y.z>` path
/// fragments, the same tokens `scripts/check-claude-version-bump.sh` greps
/// out, so this check and the detector cannot disagree about what "active"
/// means.
fn active_fixture_refs(source: &str) -> Vec<(String, String)> {
    let mut refs = Vec::new();
    for (class, _) in ACTIVE_FIXTURE_SOURCES {
        let marker = format!("fixtures/{class}_v");
        let mut rest = source;
        while let Some(at) = rest.find(&marker) {
            rest = &rest[at + marker.len()..];
            if let Some(version) = leading_version(rest) {
                refs.push((class.to_string(), version.to_string()));
            }
        }
    }
    refs
}

/// The maintenance check the reconciled fixture families owe their name to:
/// all ACTIVE evidence — the doc stamp and every fixture family a contract
/// test references — must name one Claude version, while historical fixture
/// files that no test references stay exempt. Runs everywhere `cargo test`
/// runs, with no claude binary and no live anything, so a half-landed re-pin
/// (one family re-pinned, the doc or the other family left behind) fails the
/// always-on suite, not just the CI gate.
#[test]
fn active_fixture_families_share_one_pinned_version() {
    let pin = doc_pin();
    let mut active: Vec<(String, String)> = Vec::new();

    for (_, source_rel) in ACTIVE_FIXTURE_SOURCES {
        let source =
            fs::read_to_string(repo_path(source_rel)).expect("contract test source must exist");
        let refs = active_fixture_refs(&source);
        assert!(
            !refs.is_empty(),
            "{source_rel} must reference its version-pinned fixture family"
        );
        for (class, version) in &refs {
            assert_eq!(
                version, &pin,
                "active {class} fixture is pinned to {version} but the doc stamp is {pin} — \
                 re-measure and re-pin every active family together \
                 (docs/notes/claude-contract-probes.md §Re-pin)"
            );
            // The referenced family must actually exist beside the reference.
            let prefix = format!("{class}_v{version}");
            let family_exists = fs::read_dir(repo_path("tests/fixtures"))
                .unwrap()
                .any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(&prefix)
                });
            assert!(
                family_exists,
                "active {prefix} fixture family is missing from tests/fixtures/"
            );
            active.push((class.to_string(), version.clone()));
        }
    }

    // Every version-pinned fixture file on disk is either active (referenced,
    // version-checked above) or historical (exempt by design): classify each
    // so a file can never silently fall outside the check, and prove the
    // historical families in this repo — claude_contracts 2.1.270/2.1.281,
    // stream_json_golden 2.1.270 — really are exempt, not accidentally
    // conforming.
    let mut historical: Vec<String> = Vec::new();
    for entry in fs::read_dir(repo_path("tests/fixtures")).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        let Some(class) = ACTIVE_FIXTURE_SOURCES
            .iter()
            .map(|(c, _)| *c)
            .find(|c| name.starts_with(&format!("{c}_v")))
        else {
            continue;
        };
        let version = leading_version(&name[class.len() + 2..])
            .unwrap_or_else(|| panic!("fixture {name} has no version-shaped name"));
        let is_active = active.iter().any(|(c, v)| *c == class && *v == version);
        if !is_active {
            historical.push(format!("{class}_v{version}"));
        }
    }
    assert!(
        historical.iter().any(|h| h.starts_with("claude_contracts")),
        "expected retained claude_contracts history, got {historical:?}"
    );
    assert!(
        historical
            .iter()
            .any(|h| h.starts_with("stream_json_golden")),
        "expected retained stream_json_golden history, got {historical:?}"
    );
}

/// The doc-prose half of the one-pin invariant (claudepr-893bfc4e), pinned
/// into every `cargo test` run the same way the fixture half above is: on
/// every checked line of the maintenance doc — the **Measured against:**
/// stamp, any markdown table row, any `Evidence (` preamble — every cited
/// version is the active pin or the line carries the explicit `historical`
/// attribution. So the fixtures-move/prose-lags shape of an incomplete re-pin
/// (the reverted claudepr-2e8c3884: fixtures and stamp re-pinned while the
/// tables still cited the old version) fails the always-on suite, not just
/// the CI gate. Fenced code blocks are not prose and are skipped; narrative
/// paragraphs outside the evidence tables stay outside the scope (the doc's
/// §Re-measurement history attributes its own numbers).
#[test]
fn doc_evidence_prose_upholds_the_one_pin_invariant() {
    let doc = fs::read_to_string(repo_path("docs/notes/claude-contract-probes.md"))
        .expect("docs/notes/claude-contract-probes.md must exist");
    let pin = doc_pin();

    let mut violations = Vec::new();
    let mut preambles = 0;
    let mut marked_superseded_rows = 0;
    let mut in_fence = false;
    for (idx, line) in doc.lines().enumerate() {
        let lineno = idx + 1;
        // Fences and table rows are matched on the whitespace-trimmed line —
        // the doc legitimately fences one block inside a list item at a
        // two-space indent, and markdown treats that as a fence too.
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let Some(kind) = prose_line_kind(line) else {
            continue;
        };
        if kind == "an evidence preamble" {
            preambles += 1;
        }
        let cited = cited_versions(line);
        if cited.iter().any(|v| v != &pin)
            && carries_historical_marker(line)
            && kind == "an evidence-table row"
        {
            marked_superseded_rows += 1;
        }
        for version in cited {
            if version != pin && !carries_historical_marker(line) {
                violations.push(format!(
                    "line {lineno} ({kind}) cites {version}, neither the pin {pin} \
                     nor marked historical on that line"
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "mixed-version evidence in docs/notes/claude-contract-probes.md — re-pin \
         the citations or attribute them per §Re-pin:\n  {}",
        violations.join("\n  ")
    );

    // Teeth, the `active_fixture_families_share_one_pinned_version` pattern:
    // prove the checked surfaces are live, not accidentally empty — the doc
    // really has `Evidence (` preambles in scope, and the historical-marker
    // exemption is exercised by a real superseded table citation, so a doc
    // that stopped carrying either is a scope change someone must notice.
    assert!(
        preambles > 0,
        "no `Evidence (` preamble found — prose-check scope shrank?"
    );
    assert!(
        marked_superseded_rows > 0,
        "no evidence-table row cites a superseded version under the \
         `historical` marker — the exemption path is unexercised"
    );
}

// ── Detector parse (scripts/check-claude-version-bump.sh) ────────────────────

#[test]
fn detector_current_when_stub_matches_every_active_pin() {
    let pin = doc_pin();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{pin} (Claude Code)"));

    let out = run_script("scripts/check-claude-version-bump.sh", Some(&bin), &[], &[]);

    // After the claudepr-b590e46d reconciliation every active pin — doc stamp,
    // claude_contracts, stream_json_golden — names the same version, so a
    // matching installed binary is CURRENT: the intentionally-red
    // stale-golden era (claudepr-e65ab413) is over.
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("CURRENT"), "{stdout}");
    for class in ["claude_contracts", "stream_json_golden"] {
        let line = stdout
            .lines()
            .find(|l| l.starts_with(&format!("fixture ({class}):")))
            .unwrap_or_else(|| panic!("no fixture line for {class}: {stdout}"));
        assert!(
            line.trim_end().ends_with(&pin),
            "active {class} pin must be {pin}: {line}"
        );
    }
}

#[test]
fn detector_drift_names_the_golden_repin_step() {
    let live = bumped(&doc_pin());
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{live} (Claude Code)"));

    let out = run_script("scripts/check-claude-version-bump.sh", Some(&bin), &[], &[]);

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Re-measure and re-pin every active version-pinned fixture family"),
        "{stdout}"
    );
    assert!(
        stdout.contains("stream_json_golden_v<version>.*.jsonl"),
        "{stdout}"
    );
}

/// A skeleton repo whose active pins disagree (doc + claude_contracts at one
/// version, the stream-json golden family one behind) is rejected with exit 2
/// BEFORE any claude is consulted — the divergence check needs no installed
/// binary, so a half-landed re-pin is caught even on a claude-less host.
#[test]
fn detector_rejects_divergent_active_pins_without_claude() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("scripts")).unwrap();
    fs::create_dir_all(root.join("docs/notes")).unwrap();
    fs::create_dir_all(root.join("tests/fixtures")).unwrap();
    fs::copy(
        repo_path("scripts/check-claude-version-bump.sh"),
        root.join("scripts/check-claude-version-bump.sh"),
    )
    .unwrap();
    fs::write(
        root.join("docs/notes/claude-contract-probes.md"),
        "**Measured against:** `claude` 9.9.901 (`9.9.901 (Claude Code)`), 2026-09-25\n",
    )
    .unwrap();
    fs::write(
        root.join("tests/claude_contracts.rs"),
        "const FIXTURE: &str = include_str!(\"fixtures/claude_contracts_v9.9.901.json\");\n",
    )
    .unwrap();
    // Divergent: the golden family is one patch behind the doc/contracts pin.
    fs::write(
        root.join("tests/stream_json_contract.rs"),
        concat!(
            "const INPUT: &str = include_str!(\"fixtures/stream_json_golden_v9.9.900.input.jsonl\");\n",
            "const EXPECTED: &str = include_str!(\"fixtures/stream_json_golden_v9.9.900.expected.jsonl\");\n",
            "const ERRORS: &str = include_str!(\"fixtures/stream_json_golden_v9.9.900.errors.jsonl\");\n",
        ),
    )
    .unwrap();
    for name in [
        "claude_contracts_v9.9.901.json",
        "stream_json_golden_v9.9.900.input.jsonl",
        "stream_json_golden_v9.9.900.expected.jsonl",
        "stream_json_golden_v9.9.900.errors.jsonl",
    ] {
        fs::write(root.join("tests/fixtures").join(name), "{}\n").unwrap();
    }

    let bin = root.join("bin");
    let out = Command::new("bash")
        .arg(root.join("scripts/check-claude-version-bump.sh"))
        .env("PATH", stub_path(&bin)) // no claude stub: none is needed
        .output()
        .unwrap();

    assert_eq!(
        out.status.code(),
        Some(2),
        "divergent active pins must fail closed, without claude; stderr: {}",
        stderr_of(&out)
    );
    let stderr = stderr_of(&out);
    assert!(stderr.contains("active version pins disagree"), "{stderr}");
    assert!(
        stderr.contains("9.9.901") && stderr.contains("9.9.900"),
        "{stderr}"
    );
}

/// Build a skeleton repo whose active pins all AGREE at 9.9.901 (stamp +
/// claude_contracts + stream_json_golden; `doc_body` becomes the maintenance
/// doc), for exercising the detector's prose check in isolation from drift
/// and divergence.
fn skeleton_with_agreeing_pins(root: &Path, doc_body: &str) {
    fs::create_dir_all(root.join("scripts")).unwrap();
    fs::create_dir_all(root.join("docs/notes")).unwrap();
    fs::create_dir_all(root.join("tests/fixtures")).unwrap();
    fs::copy(
        repo_path("scripts/check-claude-version-bump.sh"),
        root.join("scripts/check-claude-version-bump.sh"),
    )
    .unwrap();
    fs::write(root.join("docs/notes/claude-contract-probes.md"), doc_body).unwrap();
    fs::write(
        root.join("tests/claude_contracts.rs"),
        "const FIXTURE: &str = include_str!(\"fixtures/claude_contracts_v9.9.901.json\");\n",
    )
    .unwrap();
    fs::write(
        root.join("tests/stream_json_contract.rs"),
        concat!(
            "const INPUT: &str = include_str!(\"fixtures/stream_json_golden_v9.9.901.input.jsonl\");\n",
            "const EXPECTED: &str = include_str!(\"fixtures/stream_json_golden_v9.9.901.expected.jsonl\");\n",
            "const ERRORS: &str = include_str!(\"fixtures/stream_json_golden_v9.9.901.errors.jsonl\");\n",
        ),
    )
    .unwrap();
    for name in [
        "claude_contracts_v9.9.901.json",
        "stream_json_golden_v9.9.901.input.jsonl",
        "stream_json_golden_v9.9.901.expected.jsonl",
        "stream_json_golden_v9.9.901.errors.jsonl",
    ] {
        fs::write(root.join("tests/fixtures").join(name), "{}\n").unwrap();
    }
}

/// A skeleton whose active pins agree (all 9.9.901) while the evidence prose
/// lags — an `Evidence (` preamble citing 9.9.899 and a table row citing
/// 9.9.900, neither marked historical — is rejected with exit 2 BEFORE any
/// claude is consulted (claudepr-893bfc4e): the fixtures-move/prose-lags
/// shape of the incomplete 2.1.283 re-pin (reverted claudepr-2e8c3884) needs
/// no installed binary to be caught. The fenced row citing 9.9.888 is code,
/// not prose, and must not appear in the errors.
#[test]
fn detector_rejects_mixed_version_prose_without_claude() {
    let dir = tempfile::tempdir().unwrap();
    skeleton_with_agreeing_pins(
        dir.path(),
        concat!(
            "**Measured against:** `claude` 9.9.901 (`9.9.901 (Claude Code)`), 2026-09-26\n",
            "\n",
            "Evidence (9.9.901; P2 pairs, with a lagging 9.9.899 citation):\n",
            "\n",
            "| Run | Result |\n",
            "|---|---|\n",
            "| P2 | one firing; the 9.9.900 run behaved the same |\n",
            "\n",
            "```text\n",
            "| P2 | fenced 9.9.888 row — code blocks are not prose |\n",
            "```\n",
        ),
    );

    let bin = dir.path().join("bin");
    let out = Command::new("bash")
        .arg(dir.path().join("scripts/check-claude-version-bump.sh"))
        .env("PATH", stub_path(&bin)) // no claude stub: none may be needed
        .output()
        .unwrap();

    let stderr = stderr_of(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "mixed-version prose must fail closed, without claude; stderr: {stderr}"
    );
    assert!(stderr.contains("mixed-version evidence"), "{stderr}");
    for cited in ["9.9.899", "9.9.900"] {
        assert!(
            stderr.contains(cited),
            "the error must name the unmarked citation {cited}: {stderr}"
        );
    }
    assert!(
        stderr.contains("9.9.901"),
        "the error must name the active pin the citation disagrees with: {stderr}"
    );
    assert!(
        !stderr.contains("9.9.888"),
        "a fenced (code-block) citation is not prose and must be skipped: {stderr}"
    );
    assert!(
        !stderr.contains("claude not on PATH"),
        "the prose check must fail before claude is consulted: {stderr}"
    );
}

/// The same skeleton with the superseded citations carrying the explicit
/// `historical` marker passes the prose check — proven by reaching the next
/// failure, the absent claude, with its own exit-2 message and no
/// mixed-version error at all: the marker is the sanctioned way a re-pin
/// keeps per-version contrast rows (the doc's Arm D row does exactly this).
#[test]
fn detector_allows_historical_markers_in_prose() {
    let dir = tempfile::tempdir().unwrap();
    skeleton_with_agreeing_pins(
        dir.path(),
        concat!(
            "**Measured against:** `claude` 9.9.901 (`9.9.901 (Claude Code)`), 2026-09-26\n",
            "\n",
            "Evidence (9.9.901; P2 pairs; the historical 9.9.899 run for contrast):\n",
            "\n",
            "| Run | Result |\n",
            "|---|---|\n",
            "| P2 | one firing; the historical 9.9.900 run behaved the same |\n",
        ),
    );

    let bin = dir.path().join("bin");
    let out = Command::new("bash")
        .arg(dir.path().join("scripts/check-claude-version-bump.sh"))
        .env("PATH", stub_path(&bin)) // no claude stub: the prose check must pass first
        .output()
        .unwrap();

    let stderr = stderr_of(&out);
    // The absent-claude verdict echoes to stdout (the detector's normal
    // output stream); the prose-check errors would go to stderr.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(2),
        "exit 2 is the absent-claude verdict, not a prose failure; stderr: {stderr}"
    );
    assert!(
        stdout.contains("claude not on PATH"),
        "a fully marked doc must pass the prose check and reach the claude \
         step (stdout: {stdout}; stderr: {stderr})"
    );
    assert!(
        !stderr.contains("mixed-version"),
        "historical-marked citations are attributed, not mixed: {stderr}"
    );
}

/// A skeleton whose active pins agree across evidence sources but whose
/// stream-json golden family disagrees WITH ITSELF — the input file
/// re-pinned to 9.9.901 while expected/errors stayed at 9.9.900 — is
/// rejected with exit 2 before any claude is consulted (claudepr-b3ac3625).
/// The three files of a family are one measurement (one replay captured
/// once), so a half-moved triple is the incomplete-re-pin shape one level
/// below the cross-family divergence the test above pins. Both stale .900
/// files stay on disk so the disagreement is the only failure the detector
/// can see.
#[test]
fn detector_rejects_mixed_pins_within_one_family_without_claude() {
    let dir = tempfile::tempdir().unwrap();
    skeleton_with_agreeing_pins(
        dir.path(),
        "**Measured against:** `claude` 9.9.901 (`9.9.901 (Claude Code)`), 2026-09-26\n",
    );
    fs::write(
        dir.path().join("tests/stream_json_contract.rs"),
        concat!(
            "const INPUT: &str = include_str!(\"fixtures/stream_json_golden_v9.9.901.input.jsonl\");\n",
            "const EXPECTED: &str = include_str!(\"fixtures/stream_json_golden_v9.9.900.expected.jsonl\");\n",
            "const ERRORS: &str = include_str!(\"fixtures/stream_json_golden_v9.9.900.errors.jsonl\");\n",
        ),
    )
    .unwrap();
    for name in [
        "stream_json_golden_v9.9.900.expected.jsonl",
        "stream_json_golden_v9.9.900.errors.jsonl",
    ] {
        fs::write(dir.path().join("tests/fixtures").join(name), "{}\n").unwrap();
    }

    let bin = dir.path().join("bin");
    let out = Command::new("bash")
        .arg(dir.path().join("scripts/check-claude-version-bump.sh"))
        .env("PATH", stub_path(&bin)) // no claude stub: none may be needed
        .output()
        .unwrap();

    let stderr = stderr_of(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a family disagreeing with itself must fail closed, without claude; \
         stderr: {stderr}"
    );
    assert!(
        stderr.contains("active stream_json_golden fixture references disagree"),
        "{stderr}"
    );
    assert!(
        stderr.contains("9.9.901") && stderr.contains("9.9.900"),
        "the error must name both sides of the disagreement: {stderr}"
    );
    assert!(
        !stdout.contains("claude not on PATH"),
        "the within-family check must fail before claude is consulted \
         (stdout: {stdout})"
    );
}

/// A referenced fixture family missing from tests/fixtures/ — every active
/// stream-json golden reference points at files that do not exist — is
/// rejected with exit 2 before any claude is consulted (claudepr-b3ac3625):
/// a re-pin that moves the test references without landing the files must
/// fail as a broken repo, not silently read as drift — or worse, current.
/// The claude_contracts file stays, so the golden family is the only
/// failure named.
#[test]
fn detector_rejects_a_missing_active_fixture_family_without_claude() {
    let dir = tempfile::tempdir().unwrap();
    skeleton_with_agreeing_pins(
        dir.path(),
        "**Measured against:** `claude` 9.9.901 (`9.9.901 (Claude Code)`), 2026-09-26\n",
    );
    for suffix in ["input", "expected", "errors"] {
        fs::remove_file(
            dir.path()
                .join("tests/fixtures")
                .join(format!("stream_json_golden_v9.9.901.{suffix}.jsonl")),
        )
        .unwrap();
    }

    let bin = dir.path().join("bin");
    let out = Command::new("bash")
        .arg(dir.path().join("scripts/check-claude-version-bump.sh"))
        .env("PATH", stub_path(&bin)) // no claude stub: none may be needed
        .output()
        .unwrap();

    let stderr = stderr_of(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a missing active fixture family must fail closed, without claude; \
         stderr: {stderr}"
    );
    assert!(
        stderr.contains(
            "active fixture family is missing for \
             fixtures/stream_json_golden_v9.9.901",
        ),
        "{stderr}"
    );
    assert!(
        !stderr.contains("claude_contracts"),
        "the present claude_contracts family must not be named as missing: {stderr}"
    );
    assert!(
        !stdout.contains("claude not on PATH"),
        "the missing-family check must fail before claude is consulted \
         (stdout: {stdout})"
    );
}

#[test]
fn detector_drift_when_installed_differs_from_pin() {
    let live = bumped(&doc_pin());
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{live} (Claude Code)"));

    let out = run_script("scripts/check-claude-version-bump.sh", Some(&bin), &[], &[]);

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("DRIFT"), "{stdout}");
    assert!(stdout.contains(&live), "{stdout}");
}

#[test]
fn detector_indeterminate_without_claude() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap(); // no claude stub, minimal PATH

    let out = run_script("scripts/check-claude-version-bump.sh", Some(&bin), &[], &[]);

    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr_of(&out));
}

#[test]
fn detector_indeterminate_on_unparsable_version() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, "definitely not a version");

    let out = run_script("scripts/check-claude-version-bump.sh", Some(&bin), &[], &[]);

    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr_of(&out));
}

// ── Gate contract, hermetic (stubbed claude / gh / cargo) ────────────────────

#[test]
fn gate_current_records_evidence_and_refreshes_version_file() {
    let pin = doc_pin();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{pin} (Claude Code)"));

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), true, &[]),
        &[],
    );

    // Every active pin reconciled to one version (claudepr-b590e46d): a
    // matching installed binary is a green gate — the stale-golden era in
    // which this same run exited 1 (claudepr-e65ab413) is over.
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let evidence = dir.path().join("evidence");
    assert_eq!(status_value(&evidence, "alert"), "none");
    assert_eq!(status_value(&evidence, "contract-maintenance"), "CURRENT");
    assert_eq!(status_value(&evidence, "pinned"), pin);
    assert_eq!(status_value(&evidence, "installed"), pin);
    // Version artifact: the full first line, the same shape
    // tests/version_compat.rs::test_claude_version_recorded writes.
    assert_eq!(
        read_text(&dir.path().join("last-claude-version.txt")),
        format!("{pin} (Claude Code)\n")
    );
    // Evidence bundle shape per §Wiring: detection + probes (SKIPPED) +
    // status + next-steps; live-contract-tests.txt only when tests ran.
    let detection = read_text(&evidence.join("detection.txt"));
    assert!(detection.contains("detector-exit: 0"));
    for probe in [
        "probe-claude-contracts.sh",
        "probe-stop-toolallowed.sh",
        "probe-tui-second-turn.sh",
        "probe-stop-edge-contracts.sh",
    ] {
        let note = read_text(&evidence.join("probes").join(format!("{probe}.txt")));
        assert!(note.contains("SKIPPED"), "{probe}: {note}");
    }
    assert!(!evidence.join("live-contract-tests.txt").exists());
    assert!(evidence.join("next-steps.txt").exists());
    assert_eq!(status_value(&evidence, "follow-up"), "n/a (no drift)");
}

#[test]
fn gate_drift_exits_one_and_files_marker_issue() {
    let pin = doc_pin();
    let live = bumped(&pin);
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{live} (Claude Code)"));
    stub_gh(&bin);
    let gh_args = dir.path().join("gh-args.txt");

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), true, &["--file-follow-up"]),
        &[("GH_ARGS_FILE", gh_args.display().to_string())],
    );

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
    let evidence = dir.path().join("evidence");
    assert_eq!(status_value(&evidence, "alert"), "re-run-due");
    assert_eq!(status_value(&evidence, "pinned"), pin);
    assert_eq!(status_value(&evidence, "installed"), live);
    // The version artifact is re-anchored to the drifted version.
    assert_eq!(
        read_text(&dir.path().join("last-claude-version.txt")),
        format!("{live} (Claude Code)\n")
    );
    // Follow-up: the marker makes the issue idempotent per installed version.
    let gh = read_text(&gh_args);
    assert!(
        gh.contains(&format!("claude-contract-drift live={live}")),
        "gh invocations must carry the per-version marker:\n{gh}"
    );
    assert!(gh.contains("issue create"), "{gh}");
    assert!(
        status_value(&evidence, "follow-up").contains("filed"),
        "{}",
        status_value(&evidence, "follow-up")
    );
}

#[test]
fn gate_drift_updates_existing_issue_for_same_version() {
    let live = bumped(&doc_pin());
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{live} (Claude Code)"));
    stub_gh(&bin);
    let gh_args = dir.path().join("gh-args.txt");

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), true, &["--file-follow-up"]),
        &[
            ("GH_ARGS_FILE", gh_args.display().to_string()),
            ("GH_LIST_FOUND", "1".to_string()),
        ],
    );

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
    let gh = read_text(&gh_args);
    assert!(gh.contains("issue comment 42"), "{gh}");
    assert!(!gh.contains("issue create"), "{gh}");
    assert_eq!(
        status_value(&dir.path().join("evidence"), "follow-up"),
        "updated issue #42"
    );
}

#[test]
fn gate_drift_without_follow_up_flag_never_calls_gh() {
    let live = bumped(&doc_pin());
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{live} (Claude Code)"));
    stub_gh(&bin);
    let gh_args = dir.path().join("gh-args.txt");

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), true, &[]),
        &[("GH_ARGS_FILE", gh_args.display().to_string())],
    );

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
    assert!(
        !gh_args.exists(),
        "gh must not be invoked without --file-follow-up"
    );
    assert_eq!(
        status_value(&dir.path().join("evidence"), "follow-up"),
        "not-requested (pass --file-follow-up)"
    );
}

#[test]
fn gate_current_without_follow_up_never_calls_gh() {
    let pin = doc_pin();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{pin} (Claude Code)"));
    stub_gh(&bin);
    let gh_args = dir.path().join("gh-args.txt");

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), true, &[]),
        &[("GH_ARGS_FILE", gh_args.display().to_string())],
    );

    // CURRENT gates never touch gh — follow-up filing is drift-only, so even
    // --file-follow-up would be inert here (and is not passed).
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert!(
        !gh_args.exists(),
        "a CURRENT gate must not invoke gh for any reason"
    );
    assert_eq!(
        status_value(&dir.path().join("evidence"), "follow-up"),
        "n/a (no drift)"
    );
}

#[test]
fn gate_indeterminate_without_claude_records_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap(); // no claude stub, minimal PATH

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), true, &[]),
        &[],
    );

    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr_of(&out));
    let evidence = dir.path().join("evidence");
    assert_eq!(status_value(&evidence, "alert"), "indeterminate");
    assert_eq!(status_value(&evidence, "installed"), "unknown");
    assert_eq!(
        read_text(&dir.path().join("last-claude-version.txt")),
        "unknown\n"
    );
    assert!(read_text(&evidence.join("next-steps.txt")).contains("could not be determined"));
}

#[test]
fn gate_drift_with_failing_gh_records_failed_follow_up() {
    let live = bumped(&doc_pin());
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{live} (Claude Code)"));
    stub_gh(&bin);
    let gh_args = dir.path().join("gh-args.txt");

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), true, &["--file-follow-up"]),
        &[
            ("GH_ARGS_FILE", gh_args.display().to_string()),
            ("GH_CREATE_FAIL", "1".to_string()),
        ],
    );

    // The drift alert stands; the failed filing is recorded, never silent.
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
    assert!(status_value(&dir.path().join("evidence"), "follow-up").contains("failed"));
    assert!(!stderr_of(&out).trim().is_empty());
}

#[test]
fn gate_runs_cheap_live_contracts_when_not_skipped() {
    let pin = doc_pin();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{pin} (Claude Code)"));
    stub_cargo(&bin);
    let cargo_args = dir.path().join("cargo-args.txt");

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), false, &[]),
        &[("CARGO_ARGS_FILE", cargo_args.display().to_string())],
    );

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let evidence = dir.path().join("evidence");
    let live = read_text(&evidence.join("live-contract-tests.txt"));
    assert!(live.contains("live-tests-exit: 0"), "{live}");
    assert!(live.contains("stub cargo"), "{live}");
    assert_eq!(status_value(&evidence, "live-tests"), "ran (exit 0)");
    let cargo = read_text(&cargo_args);
    assert!(
        cargo.contains("--test claude_contracts") && cargo.contains("--ignored"),
        "gate must run the cheap live contracts verbatim: {cargo}"
    );
}

#[test]
fn gate_live_tests_skip_flag_leaves_no_transcript() {
    let pin = doc_pin();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{pin} (Claude Code)"));

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), true, &[]),
        &[],
    );

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let evidence = dir.path().join("evidence");
    assert!(!evidence.join("live-contract-tests.txt").exists());
    assert_eq!(
        status_value(&evidence, "live-tests"),
        "skipped (--skip-live-tests)"
    );
}

// ── Version-artifact location: stock and fleet layouts (no --version-file) ───
//
// The default version file must follow the same resolution
// tests/version_compat.rs applies — explicit $CLAUDE_PRINT_VERSION_ARTIFACT_DIR
// dir, else the `cargo metadata` target_directory, else the stock <repo>/target
// — so the gate's refresh and the test's write land on one file however the
// host lays out build output (bead claudepr-ec9e7480): a redirect-following
// write paired with a stock-path read (or the reverse) would strand the
// release's version artifact on every fleet host while CI stayed green.

/// A scratch root playing the wrapper's shared redirect base in the
/// fleet-layout shapes (its real value is a fleet-environment fact these
/// tests deliberately do not write out).
#[test]
fn gate_default_version_file_prefers_the_explicit_env_dir() {
    let pin = doc_pin();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{pin} (Claude Code)"));
    let redirect = dir.path().join("redirect");

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args_without_version_file(dir.path()),
        &[(
            "CLAUDE_PRINT_VERSION_ARTIFACT_DIR",
            redirect.display().to_string(),
        )],
    );

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(
        read_text(&redirect.join("last-claude-version.txt")),
        format!("{pin} (Claude Code)\n"),
        "the explicit CI-provided dir is authoritative"
    );
    // No cargo stub: under the hermetic PATH cargo is genuinely unreachable,
    // so landing in the redirect proves the explicit dir short-circuited the
    // metadata lookup entirely (a consult would have fallen back to the
    // stock layout instead).
}

#[test]
fn gate_default_version_file_follows_cargo_metadata_target_dir() {
    let pin = doc_pin();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{pin} (Claude Code)"));
    stub_cargo(&bin);
    let redirect = dir.path().join("redirect");
    let metadata_json = format!(
        "{{\"target_directory\":\"{}\",\"workspace_root\":\"/otherwise\"}}",
        redirect.display()
    );

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args_without_version_file(dir.path()),
        &[
            // Blank beats unset as the under-test shape: blank must count as
            // unset so the metadata lookup runs at all.
            ("CLAUDE_PRINT_VERSION_ARTIFACT_DIR", String::new()),
            ("CARGO_METADATA_JSON", metadata_json),
        ],
    );

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(
        read_text(&redirect.join("last-claude-version.txt")),
        format!("{pin} (Claude Code)\n"),
        "without an explicit dir the artifact must follow the target dir \
         cargo itself reports — the fleet redirect shape"
    );
}

#[test]
fn gate_default_version_file_falls_back_to_the_stock_layout_without_cargo() {
    let pin = doc_pin();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{pin} (Claude Code)"));
    // No cargo stub: the fallback's own premise (cargo metadata unavailable).

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args_without_version_file(dir.path()),
        &[("CLAUDE_PRINT_VERSION_ARTIFACT_DIR", String::new())],
    );

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let repo_target = repo_path("target").join("last-claude-version.txt");
    assert_eq!(
        read_text(&repo_target),
        format!("{pin} (Claude Code)\n"),
        "without cargo metadata the artifact must land in the stock-checkout \
         target dir the other resolvers fall back to"
    );
}

// ── Real-environment self-consistency (CI's exercise of the gate) ────────────

#[test]
fn gate_real_environment_self_consistent() {
    let dir = tempfile::tempdir().unwrap();
    // No PATH override and no --file-follow-up: exactly what CI drives, with
    // the real claude (or none) deciding the verdict.
    let gate = run_script(
        "scripts/contract-maintenance-gate.sh",
        None,
        &gate_args(dir.path(), true, &[]),
        &[],
    );
    let det = run_script("scripts/check-claude-version-bump.sh", None, &[], &[]);

    let gate_exit = gate.status.code().unwrap();
    assert_eq!(
        gate_exit,
        det.status.code().unwrap(),
        "gate exit must mirror the detector's in the real environment"
    );

    let evidence = dir.path().join("evidence");
    let alert = status_value(&evidence, "alert");
    match gate_exit {
        0 => assert_eq!(alert, "none"),
        1 => assert_eq!(alert, "re-run-due"),
        2 => assert_eq!(alert, "indeterminate"),
        other => panic!("unexpected gate exit {other}"),
    }

    // The refreshed artifact matches the real claude, or records unknown.
    let version_file = read_text(&dir.path().join("last-claude-version.txt"));
    match Command::new("claude").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let first = combined.lines().next().unwrap_or("").trim();
            assert!(!first.is_empty());
            assert_eq!(version_file.trim(), first);
        }
        _ => assert_eq!(version_file.trim(), "unknown"),
    }
}

// ── Wiring fragments: the gate cannot silently detach from CI or the docs ────

#[test]
fn ci_workflowtemplate_wires_the_gate_on_every_push() {
    let template = fs::read_to_string(repo_path("claude-print-ci-workflowtemplate.yml")).unwrap();
    for fragment in [
        // the gate itself, evidence dir, follow-up flag — both CI modes
        "bash scripts/contract-maintenance-gate.sh",
        "--evidence-dir target/contract-maintenance",
        "--file-follow-up",
        // drift is a red build (claudepr-3094ab2e): a non-zero gate exit
        // fails the run inside a fatal wrapper that names the re-pin as the
        // way back to green
        "ERROR (contract-maintenance gate): Claude contract evidence does not cover",
        "land the re-pin (docs/notes/claude-contract-probes.md §Maintenance)",
        // claude is installed first so detection compares a real version
        "https://claude.ai/install.sh",
        // release path stamps the status and refreshes the version asset
        "target/contract-maintenance/contract-status.txt",
        "cp target/last-claude-version.txt last-claude-version.txt",
    ] {
        assert!(
            template.contains(fragment),
            "WorkflowTemplate lost wiring fragment: {fragment}"
        );
    }
    // The alert-era capture is gone for good: the exit must be fatal, not
    // buffered into GATE_EXIT and swallowed.
    for absent in ["GATE_EXIT", "set +e"] {
        assert!(
            !template.contains(absent),
            "WorkflowTemplate regained the alert-era fragment {absent} — the gate exit must fail the run"
        );
    }
    // The gate is the FIRST quality gate (before fmt) and runs before the
    // verify-only early exit, so every push hits it before any other gate
    // can pass (anchored to the exit's own message — "Verify-only mode"
    // alone first appears in the clone-branch echo higher up the template).
    let gate_at = template
        .find("bash scripts/contract-maintenance-gate.sh")
        .unwrap();
    let fmt_at = template
        .find("cargo fmt --check")
        .expect("fmt gate must exist");
    let verify_at = template
        .find("Verify-only mode: all quality gates passed")
        .expect("verify-only early exit must exist");
    assert!(
        gate_at < fmt_at,
        "the contract gate must run before the fmt gate — a version change requires the re-pin first"
    );
    assert!(
        gate_at < verify_at,
        "the gate must run before the verify-only exit"
    );
}

/// The push-trigger half of "runs the gate on every push" (§Wiring,
/// claudepr-b3ac3625): the template-fragment pin above proves the *invoked*
/// template runs the gate, but a push is what *submits* it — the EventSource
/// stanza subscribes to `push` for jedarden/claude-print, the Sensor filters
/// that stream to push events on the working branch and submits a Workflow,
/// and its `workflowTemplateRef` is the hand-off to the template the
/// fragment pin guards. Any of those detaching (an eventName typo, the push
/// filter narrowed away, a templateRef renamed off the template's
/// metadata.name) silences the gate on every push while every fragment test
/// stays green. The `refs/heads/main` filter is pinned as-is: this repo
/// works directly on main, so main IS "every push" — widening or narrowing
/// it is a decision someone must notice, not a silent edit.
#[test]
fn push_sensor_wires_every_push_to_the_gate_template() {
    let sensor = fs::read_to_string(repo_path("claude-print-ci-sensor.yml")).unwrap();
    let stanza = fs::read_to_string(repo_path("claude-print-eventsource-stanza.yml")).unwrap();
    let template = fs::read_to_string(repo_path("claude-print-ci-workflowtemplate.yml")).unwrap();

    // EventSource stanza: the webhook subscription for the canonical repo
    // must include push (the only event the sensor consumes).
    for fragment in [
        "repositories:",
        "owner: jedarden",
        "- claude-print",
        "events:",
        "- push",
    ] {
        assert!(
            stanza.contains(fragment),
            "EventSource stanza lost wiring fragment: {fragment}"
        );
    }

    // Sensor: the subscription rides the github-webhooks event source under
    // the repo's own event name...
    for fragment in [
        "eventSourceName: github-webhooks",
        "eventName: claude-print",
    ] {
        assert!(
            sensor.contains(fragment),
            "Sensor lost wiring fragment: {fragment}"
        );
    }
    // ...and fires only on push to the working branch. Paths and values are
    // pinned in order inside the dependencies block, so a value cannot
    // drift onto the wrong filter.
    let deps = sensor
        .split("triggers:")
        .next()
        .expect("the dependencies block precedes triggers:");
    for fragment in [
        "headers.X-Github-Event",
        "- push",
        "body.ref",
        "refs/heads/main",
    ] {
        assert!(
            deps.contains(fragment),
            "Sensor push filter lost fragment: {fragment}"
        );
    }
    let event_at = deps.find("headers.X-Github-Event").unwrap();
    let push_at = deps.find("- push").unwrap();
    let ref_at = deps.find("body.ref").unwrap();
    let main_at = deps.find("refs/heads/main").unwrap();
    assert!(
        event_at < push_at && push_at < ref_at && ref_at < main_at,
        "the push value must follow the X-Github-Event path and the \
         refs/heads/main value the body.ref path: {deps}"
    );

    // The trigger submits a Workflow that references the gate template BY
    // NAME — and that name must stay identical to the template's own
    // metadata.name, the join the template-fragment pin above depends on.
    let triggers = sensor
        .split("triggers:")
        .nth(1)
        .expect("the triggers block follows dependencies:");
    for fragment in [
        "operation: submit",
        "kind: Workflow",
        "namespace: argo-workflows",
        "workflowTemplateRef:",
        "name: claude-print-ci",
        // the sensor-level build policy the closest siblings carry
        "concurrencyPolicy: Forbid",
    ] {
        assert!(
            triggers.contains(fragment),
            "Sensor trigger lost wiring fragment: {fragment}"
        );
    }
    // The sensor pod itself must stay right-sized (the fleet's github-push
    // sensor shape): a re-inflated request re-fights the iad-ci capacity
    // right-sizing on every sync.
    assert!(
        sensor.contains("memory: 64Mi"),
        "the sensor pod request must stay at the fleet's github-push sizing"
    );
    assert!(
        template.contains("name: claude-print-ci"),
        "the template must keep the metadata.name the sensor's \
         workflowTemplateRef joins to"
    );
    assert!(
        template.contains("entrypoint: ci"),
        "the template must keep the ci entrypoint a sensor-submitted run starts from"
    );
}

/// The clone-auth half of "invokes the gate on every push" (claudepr-
/// b3ac3625): the gate only runs if the pod first CLONES the repo, and
/// Forgejo requires auth fleet-wide — the first live run of the wired
/// push path (claude-print-ci-ltlxs, 2026-09-27) died exit 128 at an
/// anonymous clone before the gate was ever reached. The clone therefore
/// authenticates through a URL-scoped git credential helper over the
/// already-wired FORGEJO_TOKEN secret, and that helper must stay defined
/// BEFORE the generic GitHub helper: git stops at the first complete
/// credential a helper returns, so a generic-first order would answer
/// every git.ardenone.com request with GH_TOKEN and fail the clone as
/// surely as no helper at all.
#[test]
fn workflowtemplate_authenticates_the_forgejo_clone() {
    let template = fs::read_to_string(repo_path("claude-print-ci-workflowtemplate.yml")).unwrap();

    for fragment in [
        "GIT_CONFIG_COUNT",
        "credential.https://git.ardenone.com.helper",
        "username=x-token",
        "password=$FORGEJO_TOKEN",
        "name: forgejo-webhook-token",
    ] {
        assert!(
            template.contains(fragment),
            "WorkflowTemplate lost the Forgejo clone-auth fragment: {fragment}"
        );
    }

    // Order is the correctness-bearing part: the URL-scoped Forgejo helper
    // must precede the generic GitHub helper in the env list.
    let forgejo_at = template
        .find("credential.https://git.ardenone.com.helper")
        .unwrap();
    let generic_at = template
        .find("value: credential.helper\n")
        .expect("the generic GitHub credential helper must exist");
    assert!(
        forgejo_at < generic_at,
        "the Forgejo-scoped helper must precede the generic GitHub helper — \
         git stops at the first complete answer"
    );

    // Both clone arms keep the bare Forgejo URL: the token travels through
    // the helper's pipe at run time, never baked into the manifest.
    let clone_arms = template
        .matches("\"https://git.ardenone.com/jedarden/claude-print.git\" /workspace")
        .count();
    assert_eq!(
        clone_arms, 2,
        "both the release-mode and verify-only clone arms must keep the bare \
         Forgejo URL the helper authenticates (found {clone_arms})"
    );
}

// ── Gate execution: comment-aware, which the fragment pin above is not ────────
//
// `ci_workflowtemplate_wires_the_gate_on_every_push` pins the wiring with
// raw-text fragments — and a *commented-out* gate block satisfies every
// one of them: `contains`/`find` cannot tell an executing line from a `#`
// line, and commenting the gate out is exactly the move a version bump
// tempts ("temporarily unblock CI, re-pin later" — the re-pin that never
// comes). The guard below re-pins the same template comment-aware, in the
// legs the wiring contract names (claudepr-3f4aefad):
//
//   presence   an UNCOMMENTED line invokes the gate, inside the fatal
//              `if ! … then … exit 1 … fi` wrapper, with
//              `--evidence-dir` and `--file-follow-up` on executed
//              continuation lines;
//   ordering   that invocation precedes every quality gate (fmt, clippy,
//              both test legs, audit — each itself required
//              uncommented) and the verify-only green exit, so no path
//              passes the run before the gate has judged the pins, with
//              claude installed first so detection compares a real
//              version, on top of `set -ex`;
//   execution  a `#`-prefixed line does not run and satisfies nothing —
//              the leg the fragment pin lacks;
//   failure    the wrapper's failure branch exits non-zero — `exit 1`,
//              not `exit 0`, not commented out, no `|| true` neuter —
//              the wiring that turns gate exit 1 (drift) and 2 (cannot
//              determine) into a red build, the red only the §Re-pin
//              commit clears.
//
// Comment- AND token-aware by need, not taste: the template's own `#`
// comments name `cargo audit`, and its `echo "Running cargo audit..."`
// announcements carry the same substrings an executing command does —
// only consecutive-whitespace-token matching separates command from
// announcement, and only comment-skipping keeps a `#` line from
// satisfying presence or anchoring an ordering comparison.

/// A line is a comment when `#` is its first non-blank character — the
/// YAML comments and the `#` lines of the bash embedded in the
/// WorkflowTemplate's args alike. A commented-out command does not
/// execute and satisfies no presence pin.
fn is_comment_line(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// Index of the first uncommented line containing `needle`, if any.
fn uncommented_line_with(lines: &[&str], needle: &str) -> Option<usize> {
    lines
        .iter()
        .position(|line| !is_comment_line(line) && line.contains(needle))
}

/// Index of the first uncommented line whose whitespace tokens carry
/// `tokens` consecutively — the executing-command match that neither a
/// comment naming the command nor an `echo` announcing it (whose quoted
/// text often glues punctuation onto the last token) can satisfy.
fn uncommented_cmd_line(lines: &[&str], tokens: &[&str]) -> Option<usize> {
    lines.iter().position(|line| {
        if is_comment_line(line) {
            return false;
        }
        let ts: Vec<&str> = line.split_whitespace().collect();
        ts.windows(tokens.len()).any(|w| w == tokens)
    })
}

/// The quality gates the contract gate must precede, as consecutive
/// whitespace-token sequences — token matching (not substring) so the
/// template's `#` comments and `echo` announcements naming the same
/// commands cannot satisfy or anchor them.
const QUALITY_GATES: [&[&str]; 5] = [
    &["cargo", "fmt", "--check"],
    &["cargo", "clippy", "--all-targets"],
    &["cargo", "test", "--tests"],
    &["cargo", "test", "--doc"],
    &["cargo", "audit"],
];

/// The gate-execution contract over the WorkflowTemplate's text, pure so
/// the negative meta-tests below can mutate it in memory (the committed
/// non-vacuity pattern of `tests/docs_build_commands.rs`,
/// claudepr-4d967120). Panics naming the drifted leg.
fn check_ci_workflowtemplate_executes_the_gate(content: &str) {
    let lines: Vec<&str> = content.lines().collect();

    // The fail-fast foundation: with `set -e` gone no plain failure in
    // the embedded script — gate or quality gate — is fatal.
    assert!(
        uncommented_line_with(&lines, "set -ex").is_some(),
        "the CI script must keep `set -ex` — the fail-fast mode the gate's fatal \
         wrapper and every quality gate's plain failure rest on; without it a \
         failed gate leaves the run green"
    );

    // Presence, on an executing line: the gate invocation itself.
    let invocation = uncommented_line_with(&lines, "scripts/contract-maintenance-gate.sh").expect(
        "the CI WorkflowTemplate must invoke `bash scripts/contract-maintenance-gate.sh` \
             on an UNCOMMENTED line — a `#`-prefixed invocation does not run, yet it \
             satisfies every raw-text fragment the wiring pin above checks, which is \
             the hole this guard exists to close (claudepr-3f4aefad): commenting the \
             gate out is exactly how a version bump gets 'temporarily unblocked'. The \
             way back to a green build is the re-pin procedure — docs/notes/\
             claude-contract-probes.md §Re-pin — never disabling the gate",
    );

    // The invocation must stay inside the fatal wrapper: the `if !` on its
    // own line routes a non-zero gate exit into the failure branch.
    assert!(
        lines[invocation].trim_start().starts_with("if ! "),
        "the gate invocation must stay inside the fatal `if ! … then … exit 1 … fi` \
         wrapper — a bare invocation drops the named ERROR hand-off and the explicit \
         non-zero exit the §Wiring contract documents (line: {:?})",
        lines[invocation].trim()
    );

    // The wrapper's window: the invocation through its closing `fi`.
    let fi = lines[invocation + 1..]
        .iter()
        .position(|l| !is_comment_line(l) && l.trim() == "fi")
        .map(|at| invocation + 1 + at)
        .expect("the gate's `if !` wrapper must close with an uncommented `fi`");
    let window = &lines[invocation..fi];

    // The executed flags: the evidence bundle and the follow-up hand-off
    // ride the invocation's own continuation lines (the invocation line
    // included — a one-line refactor carries them there instead).
    for flag in [
        "--evidence-dir target/contract-maintenance",
        "--file-follow-up",
    ] {
        assert!(
            window
                .iter()
                .any(|l| !is_comment_line(l) && l.contains(flag)),
            "the gate invocation must keep `{flag}` on an executed (uncommented) line — \
             a commented-out flag changes what runs: without --file-follow-up drift \
             files no hand-off issue, without --evidence-dir it leaves no evidence bundle"
        );
    }

    // Failure on a Claude version bump: nothing in the wrapper may neuter
    // the gate's non-zero exit…
    for neuter in ["exit 0", "|| true"] {
        assert!(
            !window
                .iter()
                .any(|l| !is_comment_line(l) && l.contains(neuter)),
            "the gate's failure branch must not carry `{neuter}` — that is the \
             silent-unwire shape: drift detected, announced, and the run stays green"
        );
    }
    // …and it must still exit non-zero, on an executing line.
    assert!(
        window[1..]
            .iter()
            .any(|l| !is_comment_line(l) && l.trim() == "exit 1"),
        "the gate's failure branch must keep its `exit 1` on an UNCOMMENTED line — \
         that exit is the wiring that turns gate exit 1 (drift) and 2 (cannot \
         determine) into a red build; the re-pin commit (docs/notes/\
         claude-contract-probes.md §Re-pin) is what turns it green again, so the red \
         must not be removable by commenting one line"
    );

    // Ordering: the gate judges the pins before anything can pass the run.
    for tokens in QUALITY_GATES {
        let gate_line = uncommented_cmd_line(&lines, tokens).unwrap_or_else(|| {
            panic!(
                "the CI WorkflowTemplate must run `{:?}` on an UNCOMMENTED line — a \
                 commented-out quality gate silently stops running while every \
                 fragment pin stays green (the same hole the gate's own invocation \
                 had before this guard)",
                tokens.join(" ")
            )
        });
        assert!(
            invocation < gate_line,
            "the contract-maintenance gate must run BEFORE `{:?}` — the gate is the \
             FIRST quality gate (§Wiring): a version bump must fail the run before \
             any other gate can pass it",
            tokens.join(" ")
        );
    }

    // …including the verify-only green exit: no green path precedes the gate.
    let verify = uncommented_line_with(&lines, "Verify-only mode: all quality gates passed")
        .expect("the verify-only green exit must exist — the path this ordering leg anchors on");
    let green = lines[verify + 1..]
        .iter()
        .position(|l| !is_comment_line(l) && l.trim() == "exit 0")
        .map(|at| verify + 1 + at)
        .expect("the verify-only branch must keep its `exit 0` — the green path this ordering leg anchors on");
    assert!(
        invocation < verify && invocation < green,
        "the contract-maintenance gate must run BEFORE the verify-only green exit — \
         otherwise a push can pass CI without the gate ever judging its pins"
    );

    // And detection must see a real version: claude is installed first.
    let install = uncommented_line_with(&lines, "https://claude.ai/install.sh")
        .expect("CI must install the claude binary before the gate runs");
    assert!(
        install < invocation,
        "CI must install claude BEFORE invoking the gate — `claude --version` needs \
         no auth, and without the install the gate cannot compare the installed \
         version against the pins"
    );
}

#[test]
fn ci_workflowtemplate_executes_the_gate_uncommented_first_and_fatal_on_bump() {
    let template = fs::read_to_string(repo_path("claude-print-ci-workflowtemplate.yml")).unwrap();
    check_ci_workflowtemplate_executes_the_gate(&template);
}

// ── Negative meta-tests: the execution pin must FAIL when its input rots ──────

/// Run `check` and require it to panic with every fragment of `expected`
/// in the message — the failure must be the drift the mutation plants,
/// not an incidental one. The panic hook is silenced for the caught
/// unwind so expected-failure output never pollutes the log (the same
/// shape as `tests/docs_build_commands.rs`).
fn assert_drift<F>(check: F, expected: &[&str])
where
    F: FnOnce() + std::panic::UnwindSafe,
{
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = std::panic::catch_unwind(check);
    std::panic::set_hook(prev_hook);
    let message = match outcome {
        Ok(()) => panic!(
            "the mutated template PASSED the check — the execution guard is vacuous \
             for this mutation"
        ),
        Err(payload) => payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string()),
    };
    for fragment in expected {
        assert!(
            message.contains(fragment),
            "the check failed, but not for the planted drift — panic message:\n\
             {message}\nmissing fragment: {fragment:?}"
        );
    }
}

/// `text` with the single occurrence of `from` replaced by `to`. Panics on
/// any count but one, so a meta-test can never "mutate" an input the live
/// template no longer carries (or carries twice) and silently test
/// something else.
fn replaced_once(text: &str, from: &str, to: &str) -> String {
    let count = text.matches(from).count();
    assert_eq!(
        count, 1,
        "the negative meta-tests mutate {from:?} in the live template — \
         expected exactly one occurrence, found {count}"
    );
    text.replacen(from, to, 1)
}

/// Every leg of the execution pin fails on the drift shape it exists to
/// catch — each mutation edits the live template in memory (nothing is
/// written to disk) and the owning check must panic naming the planted
/// drift.
#[test]
fn negative_meta_gate_execution_drift_fails_the_execution_pin() {
    let template = fs::read_to_string(repo_path("claude-print-ci-workflowtemplate.yml")).unwrap();
    // Sanity for the live state: the committed template passes.
    check_ci_workflowtemplate_executes_the_gate(&template);

    // The invocation commented out — the "temporarily unblock CI" move
    // that satisfies every raw-text fragment pin; only the comment-aware
    // presence leg sees it.
    assert_drift(
        || {
            check_ci_workflowtemplate_executes_the_gate(&replaced_once(
                &template,
                "if ! bash scripts/contract-maintenance-gate.sh \\",
                "# if ! bash scripts/contract-maintenance-gate.sh \\",
            ))
        },
        &["UNCOMMENTED", "contract-maintenance-gate.sh"],
    );

    // The wrapper stripped down to a bare invocation.
    assert_drift(
        || {
            check_ci_workflowtemplate_executes_the_gate(&replaced_once(
                &template,
                "if ! bash scripts/contract-maintenance-gate.sh \\",
                "bash scripts/contract-maintenance-gate.sh \\",
            ))
        },
        &["if !", "wrapper"],
    );

    // The failure branch's exit 1 commented out: drift still detected and
    // announced, the run no longer fails.
    assert_drift(
        || {
            check_ci_workflowtemplate_executes_the_gate(&replaced_once(
                &template,
                "              exit 1\n            fi\n            echo \"contract-maintenance gate OK",
                "              # exit 1\n            fi\n            echo \"contract-maintenance gate OK",
            ))
        },
        &["exit 1", "red build"],
    );

    // The failure branch demoted to exit 0: the gate "fails", CI stays
    // green — the silent-unwire shape the neuter leg exists to catch.
    assert_drift(
        || {
            check_ci_workflowtemplate_executes_the_gate(&replaced_once(
                &template,
                "              exit 1\n            fi\n            echo \"contract-maintenance gate OK",
                "              exit 0\n            fi\n            echo \"contract-maintenance gate OK",
            ))
        },
        &["exit 0"],
    );

    // A `|| true` neuter glued onto the invocation line.
    assert_drift(
        || {
            check_ci_workflowtemplate_executes_the_gate(&replaced_once(
                &template,
                "if ! bash scripts/contract-maintenance-gate.sh \\",
                "if ! bash scripts/contract-maintenance-gate.sh || true \\",
            ))
        },
        &["|| true"],
    );

    // The follow-up flag commented out on its continuation line.
    assert_drift(
        || {
            check_ci_workflowtemplate_executes_the_gate(&replaced_once(
                &template,
                "              --file-follow-up; then",
                "              # --file-follow-up; then",
            ))
        },
        &["--file-follow-up"],
    );

    // The fmt gate hoisted above the contract gate: another gate now
    // passes the run before the pins are judged.
    assert_drift(
        || {
            check_ci_workflowtemplate_executes_the_gate(&replaced_once(
                &template,
                "if ! bash scripts/contract-maintenance-gate.sh \\",
                "cargo fmt --check\n            if ! bash scripts/contract-maintenance-gate.sh \\",
            ))
        },
        &["BEFORE", "cargo fmt --check"],
    );

    // The audit gate commented out: an uncommented occurrence must exist
    // or the ordering leg would compare against nothing — and the
    // template's own `# cargo audit` comment proves comment-skipping is
    // what keeps the pin honest here.
    assert_drift(
        || {
            check_ci_workflowtemplate_executes_the_gate(&replaced_once(
                &template,
                "\n            cargo audit\n",
                "\n            # cargo audit\n",
            ))
        },
        &["cargo audit", "UNCOMMENTED"],
    );

    // A green path hoisted above the gate: the verify-only exit now
    // passes pushes the gate never judged.
    assert_drift(
        || {
            check_ci_workflowtemplate_executes_the_gate(&replaced_once(
                &template,
                "if ! bash scripts/contract-maintenance-gate.sh \\",
                "echo \"Verify-only mode: all quality gates passed\"\n              exit 0\n            if ! bash scripts/contract-maintenance-gate.sh \\",
            ))
        },
        &["verify-only green exit"],
    );

    // The claude install commented out: detection compares nothing.
    assert_drift(
        || {
            check_ci_workflowtemplate_executes_the_gate(&replaced_once(
                &template,
                "if curl -fsSL https://claude.ai/install.sh | bash",
                "# if curl -fsSL https://claude.ai/install.sh | bash",
            ))
        },
        &["install", "claude"],
    );

    // The fail-fast foundation dropped.
    assert_drift(
        || {
            check_ci_workflowtemplate_executes_the_gate(&replaced_once(
                &template,
                "set -ex\n",
                "set -x\n",
            ))
        },
        &["set -ex"],
    );
}

#[test]
fn maintenance_docs_still_name_the_gate() {
    let doc = fs::read_to_string(repo_path("docs/notes/claude-contract-probes.md")).unwrap();
    let maintenance = doc
        .split("## Maintenance")
        .nth(1)
        .expect("§Maintenance section must exist");
    assert!(
        maintenance.contains("scripts/contract-maintenance-gate.sh"),
        "§Maintenance must name the gate as the executable owner"
    );
    assert!(maintenance.contains("--file-follow-up"));
    assert!(maintenance.contains("claude_contracts_v*.json"));
    assert!(maintenance.contains("stream_json_golden_v*"));

    let plan = fs::read_to_string(repo_path("docs/plan/plan.md")).unwrap();
    let r2 = plan
        .lines()
        .find(|l| l.contains("| R-2 |"))
        .expect("plan R-2 row");
    assert!(
        r2.contains("contract-maintenance-gate.sh"),
        "plan R-2 must cite the gate: {r2}"
    );

    let readme = fs::read_to_string(repo_path("README.md")).unwrap();
    assert!(readme.contains("scripts/contract-maintenance-gate.sh"));

    let agents = fs::read_to_string(repo_path("AGENTS.md")).unwrap();
    assert!(agents.contains("tests/contract_maintenance.rs"));
}

#[test]
fn gate_script_exists_and_is_executable() {
    let gate = repo_path("scripts/contract-maintenance-gate.sh");
    assert!(gate.is_file(), "gate script must exist");
    let mode = fs::metadata(&gate).unwrap().permissions().mode();
    assert_eq!(
        mode & 0o111,
        0o111,
        "the gate must be executable like the other probe scripts"
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
